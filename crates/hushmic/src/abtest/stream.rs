//! f32 audio-stream parsing for `pw-record … -` pipes.
//!
//! What pw-cat actually emits on stdout depends on the PipeWire version:
//! ≤ 1.2 writes raw headerless native-endian samples, 1.4.2+/1.6.x write
//! little-endian AU (reversed `dns.` magic, data size 0xFFFFFFFF on pipes),
//! and WAV appears only when recording to a real `.wav` path — where
//! libsndfile's IEEE-float layout is fmt(16)+fact+PEAK with the payload at
//! byte 80 (mono), and a killed writer leaves stale declared sizes. All
//! declared sizes are therefore advisory: payload is read until EOF.

use std::io::{self, Read};

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEndian {
    Le,
    Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub endian: SampleEndian,
}

/// Consume the container header, stopping at the first payload byte.
/// Unrecognized magic falls back to a raw little-endian f32 stream (pw-cat
/// ≤ 1.2 behavior) — the sniffed bytes are returned so the caller can
/// prepend them to the payload.
pub enum Header {
    Parsed(StreamInfo),
    Raw([u8; 4]),
}

pub fn read_header<R: Read>(r: &mut R) -> io::Result<Header> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    match &magic {
        b"RIFF" => read_wav(r).map(Header::Parsed),
        b".snd" => read_au(r, SampleEndian::Be).map(Header::Parsed),
        b"dns." => read_au(r, SampleEndian::Le).map(Header::Parsed),
        _ => Ok(Header::Raw(magic)),
    }
}

/// AU: 24-byte header (data offset may exceed it), fields in the byte order
/// implied by the magic. Encoding 6 = 32-bit IEEE float.
fn read_au<R: Read>(r: &mut R, e: SampleEndian) -> io::Result<StreamInfo> {
    let mut h = [0u8; 20];
    r.read_exact(&mut h)?;
    let f = |i: usize| -> u32 {
        let b = [h[i], h[i + 1], h[i + 2], h[i + 3]];
        match e {
            SampleEndian::Le => u32::from_le_bytes(b),
            SampleEndian::Be => u32::from_be_bytes(b),
        }
    };
    let (data_off, enc, rate, ch) = (f(0), f(8), f(12), f(16));
    if enc != 6 {
        return Err(bad("AU stream is not 32-bit float (encoding != 6)"));
    }
    if !(1..=64).contains(&ch) || !(4_000..=768_000).contains(&rate) {
        return Err(bad("implausible AU channel count / sample rate"));
    }
    let skip = (data_off as u64).saturating_sub(24);
    if skip > 1 << 16 {
        return Err(bad("implausible AU data offset"));
    }
    discard(r, skip)?;
    Ok(StreamInfo {
        channels: ch as u16,
        sample_rate: rate,
        endian: e,
    })
}

/// WAV: walk chunks until `data`; fmt may be 16/18/40 bytes with tag 3 or
/// EXTENSIBLE(0xFFFE) + float SubFormat. Chunk bodies are odd-padded, and a
/// budget bounds pre-data skipping (a pipe cannot seek past garbage).
fn read_wav<R: Read>(r: &mut R) -> io::Result<StreamInfo> {
    const HEADER_BUDGET: u64 = 1 << 20;
    let mut spent: u64 = 0;

    let mut rest = [0u8; 8]; // RIFF size (ignored) + "WAVE"
    r.read_exact(&mut rest)?;
    if &rest[4..8] != b"WAVE" {
        return Err(bad("RIFF stream is not WAVE"));
    }
    let mut fmt: Option<(u16, u32)> = None;
    loop {
        let mut hdr = [0u8; 8];
        r.read_exact(&mut hdr)?;
        let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        if &hdr[0..4] == b"data" {
            let (channels, sample_rate) = fmt.ok_or_else(|| bad("data chunk before fmt"))?;
            return Ok(StreamInfo {
                channels,
                sample_rate,
                endian: SampleEndian::Le,
            });
        }
        if &hdr[0..4] == b"fmt " {
            if !(16..=512).contains(&size) {
                return Err(bad("implausible fmt chunk size"));
            }
            let mut body = vec![0u8; size as usize + (size as usize & 1)];
            r.read_exact(&mut body)?;
            let u16le = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
            let tag = u16le(0);
            let channels = u16le(2);
            let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            let bits = u16le(14);
            let is_float = match tag {
                3 => true,
                0xFFFE if size >= 40 => u16le(24) == 3,
                _ => false,
            };
            if !is_float || bits != 32 {
                return Err(bad("WAV is not 32-bit IEEE float"));
            }
            if channels == 0 || channels > 64 || !(4_000..=768_000).contains(&rate) {
                return Err(bad("implausible WAV channel count / sample rate"));
            }
            fmt = Some((channels, rate));
            continue;
        }
        let skip = size as u64 + (size as u64 & 1);
        spent += skip + 8;
        if spent > HEADER_BUDGET {
            return Err(bad("gave up scanning for the data chunk"));
        }
        discard(r, skip)?;
    }
}

fn discard<R: Read>(r: &mut R, mut n: u64) -> io::Result<()> {
    let mut sink = [0u8; 4096];
    while n > 0 {
        let want = n.min(sink.len() as u64) as usize;
        let got = r.read(&mut sink[..want])?;
        if got == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        n -= got as u64;
    }
    Ok(())
}

/// Incremental f32 payload reader with a reusable buffer; partial trailing
/// bytes (writer killed mid-sample) carry over between calls and are
/// dropped at EOF.
pub struct F32Reader<R: Read> {
    src: R,
    buf: Box<[u8]>,
    carry: usize,
    endian: SampleEndian,
}

impl<R: Read> F32Reader<R> {
    pub fn new(src: R, endian: SampleEndian, chunk_bytes: usize) -> Self {
        F32Reader {
            src,
            buf: vec![0u8; chunk_bytes.max(4096)].into_boxed_slice(),
            carry: 0,
            endian,
        }
    }

    /// For the raw-stream fallback: the sniffed magic bytes are payload.
    pub fn new_with_prefix(
        src: R,
        endian: SampleEndian,
        chunk_bytes: usize,
        prefix: &[u8],
    ) -> Self {
        let mut r = Self::new(src, endian, chunk_bytes.max(prefix.len() + 4));
        r.buf[..prefix.len()].copy_from_slice(prefix);
        r.carry = prefix.len();
        r
    }

    /// One read(2) worth of samples appended into a cleared `out` (reuse it
    /// across calls). Ok(false) = clean EOF.
    pub fn read_samples(&mut self, out: &mut Vec<f32>) -> io::Result<bool> {
        out.clear();
        let n = self.src.read(&mut self.buf[self.carry..])?;
        if n == 0 {
            return Ok(false);
        }
        let total = self.carry + n;
        let whole = total - (total % 4);
        out.reserve(whole / 4);
        for q in self.buf[..whole].chunks_exact(4) {
            let b = [q[0], q[1], q[2], q[3]];
            out.push(match self.endian {
                SampleEndian::Le => f32::from_le_bytes(b),
                SampleEndian::Be => f32::from_be_bytes(b),
            });
        }
        self.buf.copy_within(whole..total, 0);
        self.carry = total - whole;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au_le(rate: u32, ch: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"dns.");
        for x in [24u32, 0xFFFF_FFFF, 6, rate, ch] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    }

    #[test]
    fn au_le_header_parses() {
        let mut data = au_le(48_000, 1);
        data.extend_from_slice(&1.0f32.to_le_bytes());
        let mut r = data.as_slice();
        match read_header(&mut r).unwrap() {
            Header::Parsed(i) => {
                assert_eq!(i.sample_rate, 48_000);
                assert_eq!(i.channels, 1);
                assert_eq!(i.endian, SampleEndian::Le);
            }
            Header::Raw(_) => panic!("AU not recognized"),
        }
        let mut rd = F32Reader::new(r, SampleEndian::Le, 4096);
        let mut out = Vec::new();
        assert!(rd.read_samples(&mut out).unwrap());
        assert_eq!(out, vec![1.0]);
    }

    #[test]
    fn wav_float_header_with_extra_chunks_parses() {
        // RIFF/WAVE + fmt(16, tag 3) + fact + PEAK-like unknown chunk + data,
        // mirroring libsndfile's f32 layout (payload at byte 80 for mono).
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&0u32.to_le_bytes()); // stale size (killed writer)
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&48_000u32.to_le_bytes());
        v.extend_from_slice(&192_000u32.to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(b"fact");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(b"PEAK");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&[0u8; 16]);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // pipe placeholder
        v.extend_from_slice(&0.5f32.to_le_bytes());
        let mut r = v.as_slice();
        match read_header(&mut r).unwrap() {
            Header::Parsed(i) => assert_eq!((i.channels, i.sample_rate), (1, 48_000)),
            Header::Raw(_) => panic!("WAV not recognized"),
        }
        let mut rd = F32Reader::new(r, SampleEndian::Le, 4096);
        let mut out = Vec::new();
        assert!(rd.read_samples(&mut out).unwrap());
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn raw_stream_falls_back_and_keeps_the_sniffed_bytes() {
        let mut v = Vec::new();
        v.extend_from_slice(&0.25f32.to_le_bytes());
        v.extend_from_slice(&(-1.0f32).to_le_bytes());
        let mut r = v.as_slice();
        let prefix = match read_header(&mut r).unwrap() {
            Header::Raw(p) => p,
            Header::Parsed(_) => panic!("raw misdetected as container"),
        };
        let mut rd = F32Reader::new_with_prefix(r, SampleEndian::Le, 4096, &prefix);
        let mut out = Vec::new();
        assert!(rd.read_samples(&mut out).unwrap());
        assert_eq!(out, vec![0.25, -1.0]);
    }

    #[test]
    fn partial_samples_carry_across_reads() {
        // A reader that returns byte-at-a-time forces the carry path.
        struct OneByte<'a>(&'a [u8], usize);
        impl Read for OneByte<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.1 >= self.0.len() {
                    return Ok(0);
                }
                buf[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let bytes: Vec<u8> = [1.5f32, -0.5, 3.25]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let mut rd = F32Reader::new(OneByte(&bytes, 0), SampleEndian::Le, 4096);
        let (mut out, mut all) = (Vec::new(), Vec::new());
        while rd.read_samples(&mut out).unwrap() {
            all.extend_from_slice(&out);
        }
        assert_eq!(all, vec![1.5, -0.5, 3.25]);
    }
}
