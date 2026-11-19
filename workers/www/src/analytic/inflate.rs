//! 수집 본문의 `ungzip(bytes)` (pako 2.2) 를 같은 받아들임으로, 그리고 고침(fix-analytic-gzip-cap)의 푼 크기 상한.
//!
//! common 의 `gzip::ungzip`(flate2)을 쓰지 않는 까닭: 원본의 ungzip 은 pako 의 inflate(windowBits 15 + 32, 머리 자동 판별)라서
//! - gzip 과 zlib 머리를 모두 받고(raw deflate 는 "incorrect header check"),
//! - gzip 멤버가 끝난 뒤 다음 바이트가 0 이 아니면 다음 멤버(gzip 이든 zlib 이든)를 이어 풀고, 0 이면 나머지를 버리며,
//!   zlib 스트림 뒤의 바이트는 무엇이든 버린다,
//! - 머리 CRC(FHCRC) · 본문 CRC32 · 길이 · Adler-32 를 확인하고, 사전(FDICT)이 필요한 zlib 은 거절한다.
//!   flate2 의 GzDecoder 는 첫 멤버만 풀고 zlib 을 받지 않는다. 그리고 상한을 두려면 푸는 도중에 멈춰야 한다.
//!
//! 해독기는 zlib 의 참조 해독기 puff(contrib/puff)의 꼴이다: 받아들이는 스트림이 zlib 과 같다(불완전한 허프만 부호는 길이 1 인 부호
//! 하나일 때만, 거리가 그 멤버에서 이미 낸 것보다 멀면 오류, 길이 · 거리 부호 286 · 287 · 30 · 31 은 오류).
//!
//! 상한의 경계는 고친 원본(`fixes/www_analytic.mjs`: pako Inflate 의 chunkSize = 상한 + 1, onData 가 상한을 넘으면 던짐)과 같게 둔다:
//! 출력 버퍼(상한 + 1 바이트)가 가득 찬 뒤 한 바이트를 더 쓰려 하면, 또는 가득 찬 채 입력이 끝나면 [`Inflate::TooLarge`].
//! 가득 찬 뒤에도 쓰지 않는 단계(블록 머리 · 표 · 끝 부호 · 꼬리 확인 · 다음 멤버의 머리)는 계속 확인하므로 그 오류는 Bad 다 (zlib 과 같다).

/// 풀기 결과의 실패
#[derive(Debug, PartialEq, Eq)]
pub enum Inflate {
    /// pako 가 던지는 경우 (머리 · 데이터 · 꼬리 오류, 잘린 입력)
    Bad,
    /// 푼 크기가 상한을 넘음
    TooLarge,
}

type Res<T> = Result<T, Inflate>;

const MAXBITS: usize = 15;
const MAXLCODES: usize = 286;
const MAXDCODES: usize = 30;
const FIXLCODES: usize = 288;

const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

struct Huffman {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// puff 의 construct: 0 = 완전, 음수 = 넘침, 양수 = 불완전
    fn new(length: &[u8]) -> (Huffman, i32) {
        let mut count = [0u16; MAXBITS + 1];
        for &l in length {
            count[l as usize] += 1;
        }
        let mut h = Huffman { count, symbol: vec![0; length.len()] };
        if h.count[0] as usize == length.len() {
            return (h, 0);
        }
        let mut left: i32 = 1;
        for len in 1..=MAXBITS {
            left <<= 1;
            left -= h.count[len] as i32;
            if left < 0 {
                return (h, left);
            }
        }
        let mut offs = [0u16; MAXBITS + 1];
        for len in 1..MAXBITS {
            offs[len + 1] = offs[len] + h.count[len];
        }
        for (sym, &l) in length.iter().enumerate() {
            if l != 0 {
                h.symbol[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        (h, left)
    }
}

/// CRC-32 (gzip)
fn crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Adler-32 (zlib)
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

struct State<'a> {
    input: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
    out: Vec<u8>,
    /// 출력 버퍼 크기 (상한 + 1)
    room: usize,
    /// 이 멤버의 출력이 시작한 곳 (거리는 멤버 안에서만)
    member_start: usize,
}

impl State<'_> {
    /// 입력이 끝났다: 출력 버퍼가 가득 찼으면 pako 는 그 버퍼를 내보내며(onData) 상한에 걸린다
    fn eof(&self) -> Inflate {
        if self.out.len() >= self.room {
            Inflate::TooLarge
        } else {
            Inflate::Bad
        }
    }

    fn byte(&mut self) -> Res<u8> {
        let b = *self.input.get(self.pos).ok_or_else(|| self.eof())?;
        self.pos += 1;
        Ok(b)
    }

    fn bits(&mut self, need: u32) -> Res<u32> {
        let mut val = self.bitbuf;
        while self.bitcnt < need {
            let b = self.byte()? as u32;
            val |= b << self.bitcnt;
            self.bitcnt += 8;
        }
        self.bitbuf = val >> need;
        self.bitcnt -= need;
        Ok(val & ((1u32 << need) - 1))
    }

    fn align(&mut self) {
        self.bitbuf = 0;
        self.bitcnt = 0;
    }

    fn write(&mut self, b: u8) -> Res<()> {
        if self.out.len() >= self.room {
            return Err(Inflate::TooLarge);
        }
        self.out.push(b);
        Ok(())
    }

    fn decode(&mut self, h: &Huffman) -> Res<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAXBITS {
            code |= self.bits(1)? as i32;
            let count = h.count[len] as i32;
            if code - count < first {
                return Ok(h.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Inflate::Bad)
    }

    fn stored(&mut self) -> Res<()> {
        self.align();
        let mut b = [0u8; 4];
        for x in &mut b {
            *x = self.byte()?;
        }
        let len = u16::from_le_bytes([b[0], b[1]]);
        let nlen = u16::from_le_bytes([b[2], b[3]]);
        if len != !nlen {
            return Err(Inflate::Bad);
        }
        for _ in 0..len {
            // zlib 의 COPY: 출력 자리가 없으면(입력이 있든 없든) 멈추고 pako 가 가득 찬 버퍼를 내보낸다
            if self.out.len() >= self.room {
                return Err(Inflate::TooLarge);
            }
            let x = self.byte()?;
            self.write(x)?;
        }
        Ok(())
    }

    fn codes(&mut self, lencode: &Huffman, distcode: &Huffman) -> Res<()> {
        loop {
            let sym = self.decode(lencode)? as usize;
            if sym < 256 {
                self.write(sym as u8)?;
            } else if sym == 256 {
                return Ok(());
            } else {
                let s = sym - 257;
                if s >= 29 {
                    return Err(Inflate::Bad);
                }
                let len = LBASE[s] as usize + self.bits(LEXT[s] as u32)? as usize;
                let d = self.decode(distcode)? as usize;
                if d >= 30 {
                    return Err(Inflate::Bad);
                }
                let dist = DBASE[d] as usize + self.bits(DEXT[d] as u32)? as usize;
                // zlib 의 MATCH 는 출력 자리를 먼저 보고(없으면 멈춤) 그다음 거리를 확인한다
                if self.out.len() >= self.room {
                    return Err(Inflate::TooLarge);
                }
                if dist > self.out.len() - self.member_start {
                    return Err(Inflate::Bad);
                }
                for _ in 0..len {
                    let x = self.out[self.out.len() - dist];
                    self.write(x)?;
                }
            }
        }
    }

    fn fixed(&mut self) -> Res<()> {
        let mut lengths = [0u8; FIXLCODES];
        for (sym, l) in lengths.iter_mut().enumerate() {
            *l = match sym {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        let (lencode, _) = Huffman::new(&lengths);
        let (distcode, _) = Huffman::new(&[5u8; MAXDCODES]);
        self.codes(&lencode, &distcode)
    }

    fn dynamic(&mut self) -> Res<()> {
        const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
        let nlen = self.bits(5)? as usize + 257;
        let ndist = self.bits(5)? as usize + 1;
        let ncode = self.bits(4)? as usize + 4;
        if nlen > MAXLCODES || ndist > MAXDCODES {
            return Err(Inflate::Bad);
        }
        let mut lengths = [0u8; MAXLCODES + MAXDCODES];
        for &o in ORDER.iter().take(ncode) {
            lengths[o] = self.bits(3)? as u8;
        }
        let (lencode, err) = Huffman::new(&lengths[..19]);
        if err != 0 {
            return Err(Inflate::Bad);
        }
        let mut index = 0;
        while index < nlen + ndist {
            let sym = self.decode(&lencode)?;
            if sym < 16 {
                lengths[index] = sym as u8;
                index += 1;
                continue;
            }
            let (len, rep) = match sym {
                16 => {
                    if index == 0 {
                        return Err(Inflate::Bad);
                    }
                    (lengths[index - 1], 3 + self.bits(2)? as usize)
                }
                17 => (0, 3 + self.bits(3)? as usize),
                _ => (0, 11 + self.bits(7)? as usize),
            };
            if index + rep > nlen + ndist {
                return Err(Inflate::Bad);
            }
            for _ in 0..rep {
                lengths[index] = len;
                index += 1;
            }
        }
        if lengths[256] == 0 {
            return Err(Inflate::Bad);
        }
        let (lencode, err) = Huffman::new(&lengths[..nlen]);
        if err < 0 || (err > 0 && nlen != (lencode.count[0] + lencode.count[1]) as usize) {
            return Err(Inflate::Bad);
        }
        let (distcode, err) = Huffman::new(&lengths[nlen..nlen + ndist]);
        if err < 0 || (err > 0 && ndist != (distcode.count[0] + distcode.count[1]) as usize) {
            return Err(Inflate::Bad);
        }
        self.codes(&lencode, &distcode)
    }

    /// raw deflate 블록들 (마지막 블록까지), 끝나면 바이트 경계로
    fn blocks(&mut self) -> Res<()> {
        loop {
            let last = self.bits(1)?;
            match self.bits(2)? {
                0 => self.stored()?,
                1 => self.fixed()?,
                2 => self.dynamic()?,
                _ => return Err(Inflate::Bad),
            }
            if last == 1 {
                self.align();
                return Ok(());
            }
        }
    }

    fn le(&mut self, n: usize) -> Res<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= (self.byte()? as u32) << (8 * i);
        }
        Ok(v)
    }

    /// gzip 머리 (1f 8b 다음부터). FHCRC 면 머리 바이트의 CRC 를 확인한다
    fn gzip_header(&mut self) -> Res<()> {
        let mut hdr: Vec<u8> = vec![0x1f, 0x8b];
        let method = self.byte()?;
        let flags = self.byte()?;
        hdr.extend([method, flags]);
        if method != 8 || flags & 0xe0 != 0 {
            return Err(Inflate::Bad);
        }
        for _ in 0..6 {
            let b = self.byte()?;
            hdr.push(b);
        }
        if flags & 0x04 != 0 {
            let lo = self.byte()?;
            let hi = self.byte()?;
            hdr.extend([lo, hi]);
            for _ in 0..u16::from_le_bytes([lo, hi]) {
                let b = self.byte()?;
                hdr.push(b);
            }
        }
        for bit in [0x08, 0x10] {
            if flags & bit != 0 {
                loop {
                    let b = self.byte()?;
                    hdr.push(b);
                    if b == 0 {
                        break;
                    }
                }
            }
        }
        if flags & 0x02 != 0 {
            let want = self.le(2)?;
            if want != crc32(&hdr, 0) & 0xffff {
                return Err(Inflate::Bad);
            }
        }
        Ok(())
    }
}

/// pako 의 `ungzip(bytes)` 에 출력 상한(`cap` 바이트)을 둔 것
pub fn ungzip_limited(input: &[u8], cap: usize) -> Res<Vec<u8>> {
    let mut s = State { input, pos: 0, bitbuf: 0, bitcnt: 0, out: Vec::new(), room: cap + 1, member_start: 0 };
    loop {
        let b0 = s.byte()?;
        let b1 = s.byte()?;
        let gzip = b0 == 0x1f && b1 == 0x8b;
        if gzip {
            s.gzip_header()?;
        } else {
            // zlib 머리: (CMF·256 + FLG) % 31 == 0, 방법 8, 창 크기 15 이하. FDICT 면 사전 id 를 읽고 "need dictionary"
            if ((b0 as u32) << 8 | b1 as u32) % 31 != 0 || b0 & 0x0f != 8 || (b0 >> 4) + 8 > 15 {
                return Err(Inflate::Bad);
            }
            if b1 & 0x20 != 0 {
                s.le(4)?;
                return Err(Inflate::Bad);
            }
        }
        s.member_start = s.out.len();
        s.blocks()?;
        let data = &s.out[s.member_start..];
        if gzip {
            let (crc, len) = (crc32(data, 0), data.len() as u32);
            if s.le(4)? != crc {
                return Err(Inflate::Bad);
            }
            if s.le(4)? != len {
                return Err(Inflate::Bad);
            }
        } else {
            let want = adler32(data);
            if s.le(4)?.swap_bytes() != want {
                return Err(Inflate::Bad);
            }
            // zlib 스트림 뒤의 바이트는 읽지 않는다
            break;
        }
        // gzip 멤버 뒤: 0 이 아닌 바이트가 이어지면 다음 멤버, 0 이거나 끝이면 멈춘다
        if s.pos >= input.len() || input[s.pos] == 0 {
            break;
        }
    }
    // 끝까지 풀었어도 출력 버퍼가 가득 찼으면 pako 는 그 버퍼를 내보내며 상한에 걸린다
    if s.out.len() > cap {
        return Err(Inflate::TooLarge);
    }
    Ok(s.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gz(data: &[u8]) -> Vec<u8> {
        premium_discount_worker_common::gzip::gzip(data)
    }

    #[test]
    fn crc_and_adler_known_values() {
        assert_eq!(crc32(b"123456789", 0), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn members_trailers_and_formats_like_pako() {
        let a = gz(br#"{"a":1}"#);
        assert_eq!(ungzip_limited(&a, 1 << 20).unwrap(), br#"{"a":1}"#);
        let two = [a.clone(), gz(b"xyz")].concat();
        assert_eq!(ungzip_limited(&two, 1 << 20).unwrap(), br#"{"a":1}xyz"#);
        assert_eq!(ungzip_limited(&[a.clone(), vec![0, 1, 2]].concat(), 1 << 20).unwrap(), br#"{"a":1}"#);
        assert_eq!(ungzip_limited(&[a.clone(), vec![1, 2, 3]].concat(), 1 << 20), Err(Inflate::Bad));
        // zlib (flate2 ZlibEncoder) 와 그 뒤의 바이트는 버림
        let mut z = flate2_like_zlib(br#"{"b":2}"#);
        assert_eq!(ungzip_limited(&z, 1 << 20).unwrap(), br#"{"b":2}"#);
        z.extend(&a);
        assert_eq!(ungzip_limited(&z, 1 << 20).unwrap(), br#"{"b":2}"#);
        let mut bad = a.clone();
        let n = bad.len();
        bad[n - 5] ^= 1;
        assert_eq!(ungzip_limited(&bad, 1 << 20), Err(Inflate::Bad));
        assert_eq!(ungzip_limited(&a[..a.len() - 4], 1 << 20), Err(Inflate::Bad));
        assert_eq!(ungzip_limited(&[], 1 << 20), Err(Inflate::Bad));
        assert_eq!(ungzip_limited(br#"{"a":1}"#, 1 << 20), Err(Inflate::Bad));
    }

    #[test]
    fn the_cap_counts_every_member() {
        let big = gz(&vec![b' '; 1000]);
        assert_eq!(ungzip_limited(&big, 1000).unwrap().len(), 1000);
        assert_eq!(ungzip_limited(&big, 999), Err(Inflate::TooLarge));
        let two = [gz(&vec![b' '; 600]), gz(&vec![b' '; 600])].concat();
        assert_eq!(ungzip_limited(&two, 1199), Err(Inflate::TooLarge));
        assert_eq!(ungzip_limited(&two, 1200).unwrap().len(), 1200);
        // 1001 바이트에서 정확히 가득 찬 뒤 꼬리가 틀리면 Bad, 잘렸으면 TooLarge (pako 와 같은 경계)
        let exact = gz(&vec![b' '; 1001]);
        let mut bad = exact.clone();
        let n = bad.len();
        bad[n - 5] ^= 1;
        assert_eq!(ungzip_limited(&bad, 1000), Err(Inflate::Bad));
        assert_eq!(ungzip_limited(&exact[..n - 2], 1000), Err(Inflate::TooLarge));
    }

    /// zlib 스트림: 78 9c + raw deflate(gzip 몸통을 빌림) + adler32 (BE)
    fn flate2_like_zlib(data: &[u8]) -> Vec<u8> {
        let g = gz(data);
        let body = &g[10..g.len() - 8];
        let mut out = vec![0x78, 0x9c];
        out.write_all(body).unwrap();
        out.extend(adler32(data).to_be_bytes());
        out
    }
}
