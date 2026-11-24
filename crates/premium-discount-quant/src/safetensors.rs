//! Zero-copy safetensors reader over `mmap` (single file or sharded with an index).

use anyhow::{bail, Context, Result};
use half::{bf16, f16};
use memmap2::Mmap;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dtype {
    F32,
    F16,
    BF16,
}

impl Dtype {
    pub fn size(self) -> usize {
        match self {
            Dtype::F32 => 4,
            Dtype::F16 | Dtype::BF16 => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub dtype: Dtype,
    pub shape: Vec<usize>,
    file: usize,
    begin: usize,
    end: usize,
}

impl TensorInfo {
    pub fn numel(&self) -> usize {
        self.shape.iter().product::<usize>().max(1)
    }
    /// Rows/cols of the tensor viewed as a matrix (last dim = cols).
    pub fn rows_cols(&self) -> (usize, usize) {
        match self.shape.len() {
            0 => (1, 1),
            _ => {
                let cols = *self.shape.last().unwrap();
                (self.numel() / cols.max(1), cols)
            }
        }
    }
}

pub struct SafeTensors {
    maps: Vec<Mmap>,
    data_start: Vec<usize>,
    pub paths: Vec<PathBuf>,
    pub tensors: BTreeMap<String, TensorInfo>,
}

impl SafeTensors {
    /// Open one or more `.safetensors` files (tensor names must be unique across them).
    pub fn open(paths: &[PathBuf]) -> Result<Self> {
        let mut st = SafeTensors {
            maps: vec![],
            data_start: vec![],
            paths: vec![],
            tensors: BTreeMap::new(),
        };
        for p in paths {
            st.add(p)?;
        }
        Ok(st)
    }

    /// Open every shard of a Hugging Face model directory (`model.safetensors.index.json` or
    /// `model.safetensors`), plus any extra files.
    pub fn open_model_dir(dir: &Path, extra: &[PathBuf]) -> Result<Self> {
        let index = dir.join("model.safetensors.index.json");
        let mut files: Vec<PathBuf> = if index.exists() {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&index)?)?;
            let mut f: Vec<String> = v["weight_map"]
                .as_object()
                .context("weight_map")?
                .values()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
            f.sort();
            f.dedup();
            f.into_iter().map(|n| dir.join(n)).collect()
        } else {
            vec![dir.join("model.safetensors")]
        };
        files.extend(extra.iter().cloned());
        Self::open(&files)
    }

    fn add(&mut self, path: &Path) -> Result<()> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        // SAFETY: the file is opened read-only and not modified while mapped.
        let map = unsafe { Mmap::map(&file)? };
        if map.len() < 8 {
            bail!("{} is too small", path.display());
        }
        let n = u64::from_le_bytes(map[..8].try_into().unwrap()) as usize;
        if n > 100 << 20 || 8 + n > map.len() {
            bail!("{}: bad header length {n}", path.display());
        }
        let header: BTreeMap<String, Value> = serde_json::from_slice(&map[8..8 + n])?;
        let fi = self.maps.len();
        for (name, v) in header {
            if name == "__metadata__" {
                continue;
            }
            let dtype = match v["dtype"].as_str() {
                Some("F32") => Dtype::F32,
                Some("F16") => Dtype::F16,
                Some("BF16") => Dtype::BF16,
                other => bail!("{name}: unsupported dtype {other:?} (only original F32/F16/BF16 weights are accepted)"),
            };
            let shape: Vec<usize> = v["shape"]
                .as_array()
                .context("shape")?
                .iter()
                .map(|x| x.as_u64().unwrap() as usize)
                .collect();
            let off = v["data_offsets"].as_array().context("offsets")?;
            let (begin, end) = (off[0].as_u64().unwrap() as usize, off[1].as_u64().unwrap() as usize);
            let info = TensorInfo {
                dtype,
                shape,
                file: fi,
                begin,
                end,
            };
            if end - begin != info.numel() * dtype.size() || 8 + n + end > map.len() {
                bail!("{name}: byte range does not match its shape");
            }
            if self.tensors.insert(name.clone(), info).is_some() {
                bail!("duplicate tensor {name}");
            }
        }
        self.data_start.push(8 + n);
        self.maps.push(map);
        self.paths.push(path.to_path_buf());
        Ok(())
    }

    pub fn info(&self, name: &str) -> Result<&TensorInfo> {
        self.tensors.get(name).with_context(|| format!("missing tensor {name}"))
    }

    /// Raw little-endian bytes of a tensor, borrowed from the map.
    pub fn bytes(&self, name: &str) -> Result<&[u8]> {
        let t = self.info(name)?;
        let s = self.data_start[t.file];
        Ok(&self.maps[t.file][s + t.begin..s + t.end])
    }

    /// Bytes of one row (zero-copy).
    pub fn row_bytes(&self, name: &str, row: usize) -> Result<&[u8]> {
        let t = self.info(name)?;
        let (rows, cols) = t.rows_cols();
        if row >= rows {
            bail!("{name}: row {row} out of range {rows}");
        }
        let w = cols * t.dtype.size();
        Ok(&self.bytes(name)?[row * w..(row + 1) * w])
    }

    /// Decode rows `[r0, r1)` to f32, appending to `out`.
    pub fn rows_f32(&self, name: &str, r0: usize, r1: usize, out: &mut Vec<f32>) -> Result<()> {
        let t = self.info(name)?;
        let (_, cols) = t.rows_cols();
        let w = cols * t.dtype.size();
        let b = &self.bytes(name)?[r0 * w..r1 * w];
        decode_into(t.dtype, b, out);
        Ok(())
    }

    /// Decode selected rows (`ids`) to f32, appending to `out`.
    pub fn gather_f32(&self, name: &str, ids: &[u32], out: &mut Vec<f32>) -> Result<()> {
        let dt = self.info(name)?.dtype;
        for &id in ids {
            decode_into(dt, self.row_bytes(name, id as usize)?, out);
        }
        Ok(())
    }

    pub fn all_f32(&self, name: &str) -> Result<Vec<f32>> {
        let t = self.info(name)?;
        let mut v = Vec::with_capacity(t.numel());
        decode_into(t.dtype, self.bytes(name)?, &mut v);
        Ok(v)
    }
}

pub fn decode_into(dt: Dtype, b: &[u8], out: &mut Vec<f32>) {
    match dt {
        Dtype::F32 => out.extend(b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap()))),
        Dtype::F16 => out.extend(b.chunks_exact(2).map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())),
        Dtype::BF16 => out.extend(b.chunks_exact(2).map(|c| bf16::from_le_bytes([c[0], c[1]]).to_f32())),
    }
}

/// Write a minimal safetensors file (used by tests and examples).
pub fn write_safetensors(path: &Path, tensors: &[(&str, Dtype, Vec<usize>, Vec<u8>)]) -> Result<()> {
    use std::io::Write;
    let mut header = serde_json::Map::new();
    let mut off = 0usize;
    for (name, dt, shape, data) in tensors {
        let dts = match dt {
            Dtype::F32 => "F32",
            Dtype::F16 => "F16",
            Dtype::BF16 => "BF16",
        };
        header.insert(
            name.to_string(),
            serde_json::json!({"dtype": dts, "shape": shape, "data_offsets": [off, off + data.len()]}),
        );
        off += data.len();
    }
    let mut h = serde_json::to_vec(&Value::Object(header))?;
    while h.len() % 8 != 0 {
        h.push(b' ');
    }
    let mut f = std::io::BufWriter::new(File::create(path)?);
    f.write_all(&(h.len() as u64).to_le_bytes())?;
    f.write_all(&h)?;
    for t in tensors {
        f.write_all(&t.3)?;
    }
    Ok(())
}

/// Streaming, crash-safe safetensors writer (Phase 2 – Part 2 D4).
/// Preserves exact source `Dtype` (`BF16`/`F16`/`F32` — never promotes dtype) while copying
/// non-vocab tensors directly from mmap and row-slicing vocab tensors via `IdRemap` with
/// minimal RAM overhead (< 64 MiB).
pub struct StreamingSafetensorsWriter {
    w: std::io::BufWriter<File>,
    part: PathBuf,
    dest: PathBuf,
    expected_bytes: usize,
    written_bytes: usize,
}

impl StreamingSafetensorsWriter {
    pub fn create(
        dest: &Path,
        decls: &[(String, Dtype, Vec<usize>)],
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<Self> {
        use std::io::Write;
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut part_os = dest.as_os_str().to_os_string();
        part_os.push(format!(".part.{}.{seq}", std::process::id()));
        let part = PathBuf::from(part_os);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut header = serde_json::Map::new();
        if let Some(meta) = metadata {
            let mut m = serde_json::Map::new();
            for (k, v) in meta {
                m.insert(k.clone(), Value::String(v.clone()));
            }
            header.insert("__metadata__".into(), Value::Object(m));
        }
        let mut off = 0usize;
        for (name, dt, shape) in decls {
            let dts = match dt {
                Dtype::F32 => "F32",
                Dtype::F16 => "F16",
                Dtype::BF16 => "BF16",
            };
            let numel: usize = shape.iter().product::<usize>().max(1);
            let nbytes = numel * dt.size();
            header.insert(
                name.clone(),
                serde_json::json!({"dtype": dts, "shape": shape, "data_offsets": [off, off + nbytes]}),
            );
            off += nbytes;
        }
        let mut h = serde_json::to_vec(&Value::Object(header))?;
        while h.len() % 8 != 0 {
            h.push(b' ');
        }
        let mut w = std::io::BufWriter::with_capacity(8 << 20, File::create(&part)?);
        w.write_all(&(h.len() as u64).to_le_bytes())?;
        w.write_all(&h)?;
        Ok(Self {
            w,
            part,
            dest: dest.to_path_buf(),
            expected_bytes: off,
            written_bytes: 0,
        })
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        self.w.write_all(bytes)?;
        self.written_bytes += bytes.len();
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        use std::io::Write;
        if self.written_bytes != self.expected_bytes {
            let _ = std::fs::remove_file(&self.part);
            bail!(
                "StreamingSafetensorsWriter: wrote {} bytes != expected {}",
                self.written_bytes,
                self.expected_bytes
            );
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

/// Repack a safetensors model (single or sharded) into a single trimmed safetensors file,
/// slicing each 2D tensor named in `vocab_tensors` along dim 0 via `remap.new_to_old` while
/// byte-copying every other tensor untouched in its original `Dtype`.
pub fn repack_safetensors_vocab(
    src_dir: &Path,
    dst_file: &Path,
    vocab_tensors: &[&str],
    remap: &crate::artifact_trim::IdRemap,
) -> Result<()> {
    let st = SafeTensors::open_model_dir(src_dir, &[])?;
    let mut decls: Vec<(String, Dtype, Vec<usize>)> = Vec::with_capacity(st.tensors.len());
    for (name, info) in &st.tensors {
        let mut shape = info.shape.clone();
        if vocab_tensors.iter().any(|&v| v == name.as_str()) {
            if shape.len() != 2 || shape[0] != remap.orig_vocab {
                bail!(
                    "repack_safetensors_vocab: tensor {name} shape {:?} incompatible with orig_vocab {}",
                    shape,
                    remap.orig_vocab
                );
            }
            shape[0] = remap.trimmed_vocab();
        }
        decls.push((name.clone(), info.dtype, shape));
    }
    let mut writer = StreamingSafetensorsWriter::create(dst_file, &decls, None)?;
    for (name, _, _) in &decls {
        if vocab_tensors.iter().any(|&v| v == name.as_str()) {
            for &old_id in &remap.new_to_old {
                writer.write_bytes(st.row_bytes(name, old_id as usize)?)?;
            }
        } else {
            writer.write_bytes(st.bytes(name)?)?;
        }
    }
    writer.finish()?;
    Ok(())
}
