//! GGUF v3 writer (streaming, crash-safe) and zero-copy reader.
//!
//! Dims are stored ggml-style (`ne[0]` = innermost/columns), i.e. reversed with respect to the
//! row-major shapes used everywhere else in this crate.

use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

pub const ALIGN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum GgmlType {
    F32 = 0,
    F16 = 1,
    Q4_0 = 2,
    Q4_1 = 3,
    Q5_0 = 6,
    Q5_1 = 7,
    Q8_0 = 8,
    Q8_1 = 9,
    Q2K = 10,
    Q3K = 11,
    Q4K = 12,
    Q5K = 13,
    Q6K = 14,
    I32 = 26,
    BF16 = 30,
}

impl GgmlType {
    pub fn from_id(id: u32) -> Result<Self> {
        Ok(match id {
            0 => GgmlType::F32,
            1 => GgmlType::F16,
            2 => GgmlType::Q4_0,
            3 => GgmlType::Q4_1,
            6 => GgmlType::Q5_0,
            7 => GgmlType::Q5_1,
            8 => GgmlType::Q8_0,
            9 => GgmlType::Q8_1,
            10 => GgmlType::Q2K,
            11 => GgmlType::Q3K,
            12 => GgmlType::Q4K,
            13 => GgmlType::Q5K,
            14 => GgmlType::Q6K,
            26 => GgmlType::I32,
            30 => GgmlType::BF16,
            _ => bail!("unsupported ggml type {id}"),
        })
    }
    pub fn block_size(self) -> usize {
        match self {
            GgmlType::F32 | GgmlType::F16 | GgmlType::BF16 | GgmlType::I32 => 1,
            GgmlType::Q4_0 | GgmlType::Q4_1 | GgmlType::Q5_0 | GgmlType::Q5_1 | GgmlType::Q8_0 | GgmlType::Q8_1 => 32,
            GgmlType::Q2K | GgmlType::Q3K | GgmlType::Q4K | GgmlType::Q5K | GgmlType::Q6K => 256,
        }
    }
    pub fn type_size(self) -> usize {
        match self {
            GgmlType::F32 | GgmlType::I32 => 4,
            GgmlType::F16 | GgmlType::BF16 => 2,
            GgmlType::Q4_0 => 18,
            GgmlType::Q4_1 => 20,
            GgmlType::Q5_0 => 22,
            GgmlType::Q5_1 => 24,
            GgmlType::Q8_0 => 34,
            GgmlType::Q8_1 => 36,
            GgmlType::Q2K => 84,
            GgmlType::Q3K => 110,
            GgmlType::Q4K => 144,
            GgmlType::Q5K => 176,
            GgmlType::Q6K => 210,
        }
    }
    pub fn row_bytes(self, cols: usize) -> usize {
        debug_assert_eq!(cols % self.block_size(), 0);
        cols / self.block_size() * self.type_size()
    }
    pub fn bpw(self) -> f32 {
        self.type_size() as f32 * 8.0 / self.block_size() as f32
    }
    pub fn name(self) -> &'static str {
        match self {
            GgmlType::F32 => "F32",
            GgmlType::F16 => "F16",
            GgmlType::BF16 => "BF16",
            GgmlType::I32 => "I32",
            GgmlType::Q4_0 => "Q4_0",
            GgmlType::Q4_1 => "Q4_1",
            GgmlType::Q5_0 => "Q5_0",
            GgmlType::Q5_1 => "Q5_1",
            GgmlType::Q8_0 => "Q8_0",
            GgmlType::Q8_1 => "Q8_1",
            GgmlType::Q2K => "Q2_K",
            GgmlType::Q3K => "Q3_K",
            GgmlType::Q4K => "Q4_K",
            GgmlType::Q5K => "Q5_K",
            GgmlType::Q6K => "Q6_K",
        }
    }
    /// `kernels::dequant_row` / `dot_row` 가 계산할 수 있는 형식인가. 나머지(Q4_0 · Q4_1 · Q5_0 · Q5_1 · Q8_1 · Q2_K · Q3_K)는
    /// 아티팩트를 다시 꾸릴 때 바이트를 그대로 옮기려고 파싱만 열어 둔 형식입니다.
    pub fn has_cpu_kernel(self) -> bool {
        matches!(
            self,
            GgmlType::F32 | GgmlType::F16 | GgmlType::BF16 | GgmlType::I32 | GgmlType::Q8_0 | GgmlType::Q4K | GgmlType::Q5K | GgmlType::Q6K
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kv {
    U32(u32),
    I32(i32),
    U64(u64),
    F32(f32),
    Bool(bool),
    Str(String),
    ArrStr(Vec<String>),
    ArrI32(Vec<i32>),
    ArrF32(Vec<f32>),
    /// Any other scalar/array type preserved verbatim with its wire type tag and raw payload bytes.
    Raw { ty: u32, payload: Vec<u8> },
    /// Legacy opaque marker.
    Other,
}

impl Kv {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Kv::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Kv::U32(v) => Some(v as u64),
            Kv::U64(v) => Some(v),
            Kv::I32(v) if v >= 0 => Some(v as u64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TensorDecl {
    pub name: String,
    pub ty: GgmlType,
    /// Row-major shape (outermost first).
    pub shape: Vec<usize>,
}

impl TensorDecl {
    pub fn nbytes(&self) -> usize {
        let cols = *self.shape.last().unwrap_or(&1);
        let rows: usize = self.shape[..self.shape.len().saturating_sub(1)].iter().product();
        rows.max(1) * self.ty.row_bytes(cols)
    }
}

fn pad(n: usize) -> usize {
    n.div_ceil(ALIGN) * ALIGN
}

fn put_str(w: &mut impl Write, s: &str) -> std::io::Result<()> {
    w.write_all(&(s.len() as u64).to_le_bytes())?;
    w.write_all(s.as_bytes())
}

fn put_kv(w: &mut impl Write, key: &str, v: &Kv) -> std::io::Result<()> {
    put_str(w, key)?;
    match v {
        Kv::U32(x) => {
            w.write_all(&4u32.to_le_bytes())?;
            w.write_all(&x.to_le_bytes())
        }
        Kv::I32(x) => {
            w.write_all(&5u32.to_le_bytes())?;
            w.write_all(&x.to_le_bytes())
        }
        Kv::F32(x) => {
            w.write_all(&6u32.to_le_bytes())?;
            w.write_all(&x.to_le_bytes())
        }
        Kv::Bool(x) => {
            w.write_all(&7u32.to_le_bytes())?;
            w.write_all(&[*x as u8])
        }
        Kv::Str(s) => {
            w.write_all(&8u32.to_le_bytes())?;
            put_str(w, s)
        }
        Kv::U64(x) => {
            w.write_all(&10u32.to_le_bytes())?;
            w.write_all(&x.to_le_bytes())
        }
        Kv::ArrStr(a) => {
            w.write_all(&9u32.to_le_bytes())?;
            w.write_all(&8u32.to_le_bytes())?;
            w.write_all(&(a.len() as u64).to_le_bytes())?;
            a.iter().try_for_each(|s| put_str(w, s))
        }
        Kv::ArrI32(a) => {
            w.write_all(&9u32.to_le_bytes())?;
            w.write_all(&5u32.to_le_bytes())?;
            w.write_all(&(a.len() as u64).to_le_bytes())?;
            a.iter().try_for_each(|x| w.write_all(&x.to_le_bytes()))
        }
        Kv::ArrF32(a) => {
            w.write_all(&9u32.to_le_bytes())?;
            w.write_all(&6u32.to_le_bytes())?;
            w.write_all(&(a.len() as u64).to_le_bytes())?;
            a.iter().try_for_each(|x| w.write_all(&x.to_le_bytes()))
        }
        Kv::Raw { ty, payload } => {
            w.write_all(&ty.to_le_bytes())?;
            w.write_all(payload)
        }
        Kv::Other => unreachable!("Other is read-only"),
    }
}

/// Streaming writer: declare every tensor up front, then feed their bytes in order.
pub struct GgufWriter {
    w: BufWriter<File>,
    part: PathBuf,
    dest: PathBuf,
    decls: Vec<TensorDecl>,
    cur: usize,
    written: usize,
}

impl GgufWriter {
    pub fn create(dest: &Path, kvs: &[(String, Kv)], decls: Vec<TensorDecl>) -> Result<Self> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut part_os = dest.as_os_str().to_os_string();
        part_os.push(format!(".part.{}.{seq}", std::process::id()));
        let part = PathBuf::from(part_os);
        let mut w = BufWriter::with_capacity(8 << 20, File::create(&part).with_context(|| format!("create {}", part.display()))?);
        w.write_all(b"GGUF")?;
        w.write_all(&3u32.to_le_bytes())?;
        w.write_all(&(decls.len() as u64).to_le_bytes())?;
        w.write_all(&((kvs.len() + 1) as u64).to_le_bytes())?;
        put_kv(&mut w, "general.alignment", &Kv::U32(ALIGN as u32))?;
        for (k, v) in kvs {
            put_kv(&mut w, k, v)?;
        }
        let mut off = 0usize;
        for d in &decls {
            put_str(&mut w, &d.name)?;
            w.write_all(&(d.shape.len().max(1) as u32).to_le_bytes())?;
            if d.shape.is_empty() {
                w.write_all(&1u64.to_le_bytes())?;
            }
            for &n in d.shape.iter().rev() {
                w.write_all(&(n as u64).to_le_bytes())?;
            }
            w.write_all(&(d.ty as u32).to_le_bytes())?;
            w.write_all(&(off as u64).to_le_bytes())?;
            off = pad(off + d.nbytes());
        }
        let pos = w.stream_position()? as usize;
        w.write_all(&vec![0u8; pad(pos) - pos])?;
        Ok(Self {
            w,
            part,
            dest: dest.to_path_buf(),
            decls,
            cur: 0,
            written: 0,
        })
    }

    pub fn decl(&self, i: usize) -> &TensorDecl {
        &self.decls[i]
    }

    /// Append bytes of the current tensor; moves on automatically when it is complete.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let need = self.decls.get(self.cur).context("all tensors already written")?.nbytes();
        if self.written + bytes.len() > need {
            bail!("{}: got more than {need} bytes", self.decls[self.cur].name);
        }
        self.w.write_all(bytes)?;
        self.written += bytes.len();
        if self.written == need {
            self.w.write_all(&vec![0u8; pad(need) - need])?;
            self.cur += 1;
            self.written = 0;
        }
        Ok(())
    }

    pub fn current(&self) -> Option<&TensorDecl> {
        self.decls.get(self.cur)
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        if self.cur != self.decls.len() {
            bail!("only {}/{} tensors written", self.cur, self.decls.len());
        }
        self.w.flush()?;
        self.w.get_ref().sync_all()?;
        drop(self.w);
        match std::fs::rename(&self.part, &self.dest) {
            Ok(()) => {}
            Err(_) if self.dest.exists() => {
                let mut bak_os = self.dest.as_os_str().to_os_string();
                bak_os.push(format!(".bak.{}", std::process::id()));
                let bak = PathBuf::from(bak_os);
                let _ = std::fs::remove_file(&bak);
                std::fs::rename(&self.dest, &bak)?;
                if let Err(e) = std::fs::rename(&self.part, &self.dest) {
                    let _ = std::fs::rename(&bak, &self.dest);
                    return Err(e.into());
                }
                let _ = std::fs::remove_file(&bak);
            }
            Err(e) => return Err(e.into()),
        }
        Ok(self.dest)
    }
}

#[derive(Debug, Clone)]
pub struct GgufTensor {
    pub ty: GgmlType,
    /// Row-major shape.
    pub shape: Vec<usize>,
    pub offset: usize,
}

impl GgufTensor {
    pub fn rows_cols(&self) -> (usize, usize) {
        let cols = *self.shape.last().unwrap_or(&1);
        (self.shape.iter().product::<usize>() / cols.max(1), cols)
    }
    pub fn nbytes(&self) -> usize {
        let (r, c) = self.rows_cols();
        r * self.ty.row_bytes(c)
    }
}

/// Memory-mapped GGUF file; tensor/row accessors borrow directly from the map.
pub struct GgufFile {
    map: Mmap,
    pub path: PathBuf,
    data_start: usize,
    pub kvs: BTreeMap<String, Kv>,
    pub tensors: BTreeMap<String, GgufTensor>,
    pub order: Vec<String>,
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            bail!("truncated GGUF");
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn string(&mut self) -> Result<String> {
        let n = self.u64()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn scalar_size(t: u32) -> Option<usize> {
        Some(match t {
            0 | 1 | 7 => 1,
            2 | 3 => 2,
            4..=6 => 4,
            10..=12 => 8,
            _ => return None,
        })
    }
    fn value(&mut self, t: u32) -> Result<Kv> {
        Ok(match t {
            4 => Kv::U32(self.u32()?),
            5 => Kv::I32(self.u32()? as i32),
            6 => Kv::F32(f32::from_bits(self.u32()?)),
            7 => Kv::Bool(self.take(1)?[0] != 0),
            8 => Kv::Str(self.string()?),
            10 => Kv::U64(self.u64()?),
            9 => {
                let start = self.p;
                let et = self.u32()?;
                let n = self.u64()? as usize;
                match et {
                    8 => Kv::ArrStr((0..n).map(|_| self.string()).collect::<Result<_>>()?),
                    5 => Kv::ArrI32((0..n).map(|_| self.u32().map(|v| v as i32)).collect::<Result<_>>()?),
                    6 => Kv::ArrF32((0..n).map(|_| self.u32().map(f32::from_bits)).collect::<Result<_>>()?),
                    _ => {
                        let s = Self::scalar_size(et).context("nested array")?;
                        self.take(n * s)?;
                        Kv::Raw { ty: 9, payload: self.b[start..self.p].to_vec() }
                    }
                }
            }
            other => {
                let s = Self::scalar_size(other).context("bad kv type")?;
                let bytes = self.take(s)?.to_vec();
                Kv::Raw { ty: other, payload: bytes }
            }
        })
    }
}

impl GgufFile {
    pub fn open(path: &Path) -> Result<Self> {
        let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
        // SAFETY: read-only map of a file we do not modify.
        let map = unsafe { Mmap::map(&f)? };
        let mut c = Cur { b: &map, p: 0 };
        if c.take(4)? != b"GGUF" {
            bail!("not a GGUF file");
        }
        let ver = c.u32()?;
        if ver != 3 {
            bail!("GGUF version {ver} unsupported");
        }
        let nt = c.u64()? as usize;
        let nk = c.u64()? as usize;
        let mut kvs = BTreeMap::new();
        for _ in 0..nk {
            let k = c.string()?;
            let t = c.u32()?;
            kvs.insert(k, c.value(t)?);
        }
        let align = kvs.get("general.alignment").and_then(Kv::as_u64).unwrap_or(ALIGN as u64) as usize;
        let mut tensors = BTreeMap::new();
        let mut order = Vec::new();
        for _ in 0..nt {
            let name = c.string()?;
            let nd = c.u32()? as usize;
            let mut dims: Vec<usize> = (0..nd).map(|_| c.u64().map(|v| v as usize)).collect::<Result<_>>()?;
            dims.reverse();
            let ty = GgmlType::from_id(c.u32()?)?;
            let offset = c.u64()? as usize;
            order.push(name.clone());
            tensors.insert(name, GgufTensor { ty, shape: dims, offset });
        }
        let data_start = c.p.div_ceil(align) * align;
        let g = GgufFile {
            map,
            path: path.to_path_buf(),
            data_start,
            kvs,
            tensors,
            order,
        };
        for (n, t) in &g.tensors {
            if g.data_start + t.offset + t.nbytes() > g.map.len() {
                bail!("{n}: data out of bounds");
            }
        }
        Ok(g)
    }

    pub fn tensor(&self, name: &str) -> Result<&GgufTensor> {
        self.tensors.get(name).with_context(|| format!("missing tensor {name}"))
    }

    pub fn tensor_bytes(&self, name: &str) -> Result<&[u8]> {
        let t = self.tensor(name)?;
        let s = self.data_start + t.offset;
        Ok(&self.map[s..s + t.nbytes()])
    }

    /// Zero-copy row gather: the quantized bytes of one row, straight from the page cache.
    pub fn row_bytes(&self, name: &str, row: usize) -> Result<&[u8]> {
        let t = self.tensor(name)?;
        let (rows, cols) = t.rows_cols();
        if row >= rows {
            bail!("{name}: row {row} >= {rows}");
        }
        let rb = t.ty.row_bytes(cols);
        let s = self.data_start + t.offset + row * rb;
        Ok(&self.map[s..s + rb])
    }

    pub fn i32_values(&self, name: &str) -> Result<Vec<i32>> {
        let t = self.tensor(name)?;
        if t.ty != GgmlType::I32 {
            bail!("{name} is not I32");
        }
        Ok(self
            .tensor_bytes(name)?
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }

    pub fn f32_values(&self, name: &str) -> Result<Vec<f32>> {
        let t = self.tensor(name)?;
        if !t.ty.has_cpu_kernel() {
            bail!("{name}: no CPU dequant kernel for {}", t.ty.name());
        }
        let (r, c) = t.rows_cols();
        let mut out = vec![0f32; r * c];
        for i in 0..r {
            crate::kernels::dequant_row(t.ty, self.row_bytes(name, i)?, &mut out[i * c..(i + 1) * c]);
        }
        Ok(out)
    }

    pub fn kv_str(&self, key: &str) -> Option<&str> {
        self.kvs.get(key).and_then(Kv::as_str)
    }

    pub fn file_len(&self) -> usize {
        self.map.len()
    }
}

/// Optional remapped tokenizer metadata overrides when repacking a GGUF file.
#[derive(Debug, Clone, Default)]
pub struct GgufVocabOverride {
    pub tokens: Option<Vec<String>>,
    pub token_types: Option<Vec<i32>>,
    pub merges: Option<Vec<String>>,
    pub extra_kvs: Vec<(String, Kv)>,
}

/// Zero-requantization GGUF vocabulary repacker (Phase 2 – Part 2 D4).
///
/// Copies every backbone tensor verbatim byte-for-byte from `src_path`, slices `token_embd.weight`
/// (and `output.weight` if untied and matching `orig_vocab`) strictly by `remap.new_to_old`
/// (`E0` bit-exact on kept rows), remaps embedded `tokenizer.ggml.*` and `qwen35.*_token_id`
/// metadata, and records `logis.vocab.orig_ids` (`ArrI32`).
pub fn repack_gguf_vocab(
    src_path: &Path,
    dst_path: &Path,
    remap: &crate::artifact_trim::IdRemap,
    overrides: &GgufVocabOverride,
) -> Result<()> {
    let src = GgufFile::open(src_path)?;
    let k = remap.trimmed_vocab();
    let orig_v = remap.orig_vocab;

    let mut kv_map: BTreeMap<String, Kv> = BTreeMap::new();
    let kept_tok_set: HashSet<String> = if let Some(toks) = &overrides.tokens {
        toks.iter().cloned().collect()
    } else if let Some(Kv::ArrStr(arr)) = src.kvs.get("tokenizer.ggml.tokens") {
        remap
            .new_to_old
            .iter()
            .filter_map(|&old| arr.get(old as usize).cloned())
            .collect()
    } else {
        HashSet::new()
    };
    let special_id_keys = [
        "tokenizer.ggml.bos_token_id",
        "tokenizer.ggml.eos_token_id",
        "tokenizer.ggml.padding_token_id",
        "tokenizer.ggml.unknown_token_id",
        "qwen35.vision_start_token_id",
        "qwen35.vision_end_token_id",
        "qwen35.image_token_id",
        "qwen35.video_token_id",
    ];
    for (key, val) in &src.kvs {
        if key == "general.alignment" {
            continue;
        }
        if key == "tokenizer.ggml.tokens" {
            if let Some(toks) = &overrides.tokens {
                kv_map.insert(key.clone(), Kv::ArrStr(toks.clone()));
            } else if let Kv::ArrStr(arr) = val {
                let sliced: Vec<String> = remap
                    .new_to_old
                    .iter()
                    .map(|&old| arr.get(old as usize).cloned().unwrap_or_default())
                    .collect();
                kv_map.insert(key.clone(), Kv::ArrStr(sliced));
            }
            continue;
        }
        if key == "tokenizer.ggml.token_type" {
            if let Some(tt) = &overrides.token_types {
                kv_map.insert(key.clone(), Kv::ArrI32(tt.clone()));
            } else if let Kv::ArrI32(arr) = val {
                let sliced: Vec<i32> = remap
                    .new_to_old
                    .iter()
                    .map(|&old| arr.get(old as usize).copied().unwrap_or(1))
                    .collect();
                kv_map.insert(key.clone(), Kv::ArrI32(sliced));
            }
            continue;
        }
        if key == "tokenizer.ggml.scores" {
            if let Kv::ArrF32(arr) = val {
                let sliced: Vec<f32> = remap
                    .new_to_old
                    .iter()
                    .map(|&old| arr.get(old as usize).copied().unwrap_or(0.0))
                    .collect();
                kv_map.insert(key.clone(), Kv::ArrF32(sliced));
            }
            continue;
        }
        if key == "tokenizer.ggml.merges" {
            if let Some(m) = &overrides.merges {
                kv_map.insert(key.clone(), Kv::ArrStr(m.clone()));
            } else if let Kv::ArrStr(arr) = val {
                let filtered: Vec<String> = arr
                    .iter()
                    .filter(|entry| {
                        let Some((a, b)) = entry.split_once(' ') else { return false };
                        if !kept_tok_set.contains(a) || !kept_tok_set.contains(b) {
                            return false;
                        }
                        let mut ab = String::with_capacity(a.len() + b.len());
                        ab.push_str(a);
                        ab.push_str(b);
                        kept_tok_set.contains(&ab)
                    })
                    .cloned()
                    .collect();
                kv_map.insert(key.clone(), Kv::ArrStr(filtered));
            } else {
                kv_map.insert(key.clone(), Kv::ArrStr(Vec::new()));
            }
            continue;
        }
        if special_id_keys.contains(&key.as_str()) {
            if let Some(old_u64) = val.as_u64() {
                if let Some(new_id) = remap.map_old(old_u64 as u32) {
                    kv_map.insert(key.clone(), Kv::U32(new_id));
                } else {
                    bail!("repack_gguf_vocab: GGUF special key {key}={old_u64} dropped from vocab");
                }
            }
            continue;
        }
        if !matches!(val, Kv::Other) {
            kv_map.insert(key.clone(), val.clone());
        }
    }
    // Sync logis.v_head_order alongside alphaedge.v_head_order if present
    if let Some(Kv::Str(v)) = kv_map.get("alphaedge.v_head_order").cloned() {
        kv_map.entry("logis.v_head_order".into()).or_insert(Kv::Str(v));
    }
    kv_map.insert(
        "logis.vocab.orig_ids".into(),
        Kv::ArrI32(remap.new_to_old.iter().map(|&id| id as i32).collect()),
    );
    kv_map.insert("logis.vocab.orig_size".into(), Kv::U32(orig_v as u32));
    kv_map.insert("logis.vocab.trimmed_size".into(), Kv::U32(k as u32));
    for (ek, ev) in &overrides.extra_kvs {
        kv_map.insert(ek.clone(), ev.clone());
    }

    let kvs: Vec<(String, Kv)> = kv_map.into_iter().collect();
    let mut decls: Vec<TensorDecl> = Vec::with_capacity(src.order.len());
    let mut sliced_names: Vec<String> = Vec::new();
    for name in &src.order {
        if name.starts_with("mtp.") {
            continue;
        }
        let t = src.tensor(name)?;
        let mut shape = t.shape.clone();
        let is_vocab_mat = (name == "token_embd.weight" || name == "output.weight")
            && shape.len() == 2
            && shape[0] == orig_v;
        if is_vocab_mat {
            shape[0] = k;
            sliced_names.push(name.clone());
        }
        decls.push(TensorDecl {
            name: name.clone(),
            ty: t.ty,
            shape,
        });
    }
    if sliced_names.is_empty() {
        bail!(
            "repack_gguf_vocab: no token_embd.weight/output.weight with rows={orig_v} in {}",
            src_path.display()
        );
    }

    if let Some(parent) = dst_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = GgufWriter::create(dst_path, &kvs, decls)?;
    for name in &src.order {
        if name.starts_with("mtp.") {
            continue;
        }
        if sliced_names.iter().any(|s| s == name) {
            for &old_id in &remap.new_to_old {
                writer.write(src.row_bytes(name, old_id as usize)?)?;
            }
        } else {
            writer.write(src.tensor_bytes(name)?)?;
        }
    }
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_trim::{IdRemap, NumberingMode};

    #[test]
    fn unit_gguf_and_safetensors_row_bit_exact() {
        let tmp_dir = std::env::temp_dir().join(format!("pd_repack_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_dir);

        // 1. Mini Q8_0 GGUF: token_embd [128, 32] + norm [32]
        let src_gguf = tmp_dir.join("src.gguf");
        let dst_gguf = tmp_dir.join("dst.gguf");
        let rows = 128usize;
        let cols = 32usize;
        let rb = GgmlType::Q8_0.row_bytes(cols);
        let emb_bytes: Vec<u8> = (0..rows * rb).map(|i| ((i * 31 + 7) % 251) as u8).collect();
        let norm_bytes: Vec<u8> = (0..cols * 4).map(|i| (i % 256) as u8).collect();
        let kvs = vec![
            ("general.architecture".into(), Kv::Str("qwen35".into())),
            ("tokenizer.ggml.tokens".into(), Kv::ArrStr((0..rows).map(|i| format!("t{i}")).collect())),
            ("tokenizer.ggml.token_type".into(), Kv::ArrI32(vec![1; rows])),
            ("tokenizer.ggml.eos_token_id".into(), Kv::U32(10)),
            ("qwen35.image_token_id".into(), Kv::U32(20)),
        ];
        let decls = vec![
            TensorDecl { name: "token_embd.weight".into(), ty: GgmlType::Q8_0, shape: vec![rows, cols] },
            TensorDecl { name: "output_norm.weight".into(), ty: GgmlType::F32, shape: vec![cols] },
        ];
        let mut w = GgufWriter::create(&src_gguf, &kvs, decls).unwrap();
        w.write(&emb_bytes).unwrap();
        w.write(&norm_bytes).unwrap();
        w.finish().unwrap();

        let kept: Vec<u32> = (0..64).map(|i| i * 2).collect();
        let remap = IdRemap::from_kept_ids(rows, &kept, NumberingMode::Contiguous).unwrap();
        repack_gguf_vocab(&src_gguf, &dst_gguf, &remap, &GgufVocabOverride::default()).unwrap();

        let out = GgufFile::open(&dst_gguf).unwrap();
        assert_eq!(out.tensor("token_embd.weight").unwrap().shape, vec![64, 32]);
        assert_eq!(out.tensor_bytes("output_norm.weight").unwrap(), norm_bytes.as_slice());
        for (new_id, &old_id) in kept.iter().enumerate() {
            assert_eq!(
                out.row_bytes("token_embd.weight", new_id).unwrap(),
                &emb_bytes[old_id as usize * rb..(old_id as usize + 1) * rb],
                "GGUF row {new_id} (old {old_id}) must be E0 bit-exact"
            );
        }
        assert_eq!(out.kvs.get("tokenizer.ggml.eos_token_id"), Some(&Kv::U32(5)));
        assert_eq!(out.kvs.get("qwen35.image_token_id"), Some(&Kv::U32(10)));

        // 2. Mini BF16 safetensors repack bit-exact
        let st_dir = tmp_dir.join("st_src");
        let _ = std::fs::create_dir_all(&st_dir);
        let st_src = st_dir.join("model.safetensors");
        let st_dst = tmp_dir.join("st_dst.safetensors");
        let st_emb: Vec<u8> = (0..rows * cols * 2).map(|i| ((i * 17 + 3) % 253) as u8).collect();
        crate::safetensors::write_safetensors(
            &st_src,
            &[
                ("embeddings.tok_embeddings.weight", crate::safetensors::Dtype::BF16, vec![rows, cols], st_emb.clone()),
                ("final_norm.weight", crate::safetensors::Dtype::F32, vec![cols], norm_bytes.clone()),
            ],
        ).unwrap();
        crate::safetensors::repack_safetensors_vocab(&st_dir, &st_dst, &["embeddings.tok_embeddings.weight"], &remap).unwrap();
        let st_out = crate::safetensors::SafeTensors::open(&[st_dst.clone()]).unwrap();
        assert_eq!(st_out.info("embeddings.tok_embeddings.weight").unwrap().dtype, crate::safetensors::Dtype::BF16);
        assert_eq!(st_out.info("embeddings.tok_embeddings.weight").unwrap().shape, vec![64, 32]);
        assert_eq!(st_out.bytes("final_norm.weight").unwrap(), norm_bytes.as_slice());
        for (new_id, &old_id) in kept.iter().enumerate() {
            let w_b = cols * 2;
            assert_eq!(
                st_out.row_bytes("embeddings.tok_embeddings.weight", new_id).unwrap(),
                &st_emb[old_id as usize * w_b..(old_id as usize + 1) * w_b]
            );
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    /// 파싱만 열어 둔 형식(Q4_0 등)은 `f32_values` 에서 패닉하지 않고 오류를 돌려준다.
    #[test]
    fn parse_only_type_is_an_error_not_a_panic() {
        let tmp_dir = std::env::temp_dir().join(format!("pd_parse_only_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_dir);
        let path = tmp_dir.join("q4_0.gguf");
        let (rows, cols) = (2usize, 32usize);
        let decls = vec![TensorDecl { name: "w".into(), ty: GgmlType::Q4_0, shape: vec![rows, cols] }];
        let mut w = GgufWriter::create(&path, &[], decls).unwrap();
        w.write(&vec![0u8; rows * GgmlType::Q4_0.row_bytes(cols)]).unwrap();
        w.finish().unwrap();

        let g = GgufFile::open(&path).unwrap();
        assert!(!GgmlType::Q4_0.has_cpu_kernel());
        assert!(GgmlType::Q8_0.has_cpu_kernel());
        assert!(g.f32_values("w").is_err());
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn repack_gguf_vocab_preserves_merges_and_strips_mtp() {
        let tmp_dir = std::env::temp_dir().join(format!("pd_repack_merge_mtp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();

        let src_gguf = tmp_dir.join("src.gguf");
        let dst_gguf = tmp_dir.join("dst.gguf");
        let (rows, cols) = (8usize, 32usize);
        let rb = GgmlType::Q8_0.row_bytes(cols);
        let emb_bytes = vec![1u8; rows * rb];
        let mtp_bytes = vec![2u8; rows * rb];
        let norm_bytes = vec![0u8; cols * 4];
        let tokens: Vec<String> = vec!["a", "b", "ab", "c", "d", "cd", "e", "f"]
            .into_iter()
            .map(String::from)
            .collect();
        let kvs = vec![
            ("general.architecture".into(), Kv::Str("qwen35".into())),
            ("tokenizer.ggml.tokens".into(), Kv::ArrStr(tokens)),
            ("tokenizer.ggml.merges".into(), Kv::ArrStr(vec!["a b".into(), "c d".into(), "e f".into()])),
            ("tokenizer.ggml.eos_token_id".into(), Kv::U32(0)),
        ];
        let decls = vec![
            TensorDecl { name: "token_embd.weight".into(), ty: GgmlType::Q8_0, shape: vec![rows, cols] },
            TensorDecl { name: "mtp.head.weight".into(), ty: GgmlType::Q8_0, shape: vec![rows, cols] },
            TensorDecl { name: "output_norm.weight".into(), ty: GgmlType::F32, shape: vec![cols] },
        ];
        let mut w = GgufWriter::create(&src_gguf, &kvs, decls).unwrap();
        w.write(&emb_bytes).unwrap();
        w.write(&mtp_bytes).unwrap();
        w.write(&norm_bytes).unwrap();
        w.finish().unwrap();

        // Keep only ids [0("a"), 1("b"), 2("ab"), 3("c")] -> "a b" survives, "c d" and "e f" dropped, mtp.* stripped
        let remap = IdRemap::from_kept_ids(rows, &[0, 1, 2, 3], NumberingMode::Contiguous).unwrap();
        repack_gguf_vocab(&src_gguf, &dst_gguf, &remap, &GgufVocabOverride::default()).unwrap();

        let out = GgufFile::open(&dst_gguf).unwrap();
        assert_eq!(out.kvs.get("tokenizer.ggml.merges"), Some(&Kv::ArrStr(vec!["a b".into()])));
        assert!(!out.order.iter().any(|n| n.starts_with("mtp.")));
        assert_eq!(out.tensor("token_embd.weight").unwrap().shape, vec![4, 32]);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
