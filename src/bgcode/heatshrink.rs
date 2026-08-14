//! Heatshrink, the LZSS variant that carries the G-code blocks of a binary
//! G-code file.
//!
//! The stream has no header of its own: a `1` tag bit introduces an eight-bit
//! literal, a `0` tag bit a back reference whose distance and length are stored
//! one less than their real value. Bits are packed most-significant first and
//! the final byte is padded with zeros, so decoding stops on the byte count the
//! block header promised rather than on the end of the input.
//!
//! Only the window and lookahead pairs named by the bgcode specification occur.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    window: u32,
    lookahead: u32,
}

impl Params {
    pub const W11: Self = Self {
        window: 11,
        lookahead: 4,
    };
    pub const W12: Self = Self {
        window: 12,
        lookahead: 4,
    };

    /// The most bytes one stored byte can unpack to. A back reference is the
    /// only way to gain: it spends a tag bit plus a window and a lookahead
    /// field to name at most `2^lookahead` bytes, and a literal costs nine bits
    /// for eight. Rounded up, so a stream sitting exactly on the bound still
    /// decodes — 8:1 for W11, where the arithmetic is exact, and 7.53:1 for
    /// W12.
    pub fn expansion(self) -> usize {
        let longest = 1usize << self.lookahead;
        (longest * 8).div_ceil(1 + self.window as usize + self.lookahead as usize)
    }
}

/// Returns `None` if the stream ends before `expected` bytes are produced.
pub fn decode(source: &[u8], params: Params, expected: usize) -> Option<Vec<u8>> {
    // `expected` is four bytes out of the file, so only what this many stored
    // bytes could actually produce is reserved for.
    let room = expected.min(source.len().saturating_mul(params.expansion()));
    let mut out = Vec::with_capacity(room);
    let mut bits = Reader {
        bytes: source,
        at: 0,
    };

    while out.len() < expected {
        if bits.read(1)? == 1 {
            out.push(bits.read(8)? as u8);
            continue;
        }
        let distance = bits.read(params.window)? as usize + 1;
        let length = bits.read(params.lookahead)? as usize + 1;
        let from = out.len().checked_sub(distance)?;
        for offset in 0..length {
            if out.len() == expected {
                break;
            }
            // Byte at a time, so a reference may overlap what it is writing.
            out.push(out[from + offset]);
        }
    }
    Some(out)
}

pub fn encode(source: &[u8], params: Params) -> Vec<u8> {
    let mut writer = Writer::default();
    let mut matcher = Matcher::new(source, params);
    let mut at = 0;

    while at < source.len() {
        match matcher.longest(at) {
            Some((distance, length)) => {
                writer.write(0, 1);
                writer.write(distance as u32 - 1, params.window);
                writer.write(length as u32 - 1, params.lookahead);
                at += length;
            }
            None => {
                writer.write(1, 1);
                writer.write(u32::from(source[at]), 8);
                at += 1;
            }
        }
    }
    writer.finish()
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn read(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            let byte = *self.bytes.get(self.at / 8)?;
            value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Some(value)
    }
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    byte: u8,
    filled: u32,
}

impl Writer {
    fn write(&mut self, value: u32, count: u32) {
        for shift in (0..count).rev() {
            self.byte = (self.byte << 1) | ((value >> shift) & 1) as u8;
            self.filled += 1;
            if self.filled == 8 {
                self.out.push(self.byte);
                self.byte = 0;
                self.filled = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.out.push(self.byte << (8 - self.filled));
        }
        self.out
    }
}

const MIN_MATCH: usize = 3;
const CHAIN_LIMIT: usize = 32;
const HASH_BITS: u32 = 15;

/// Hash chains over three-byte prefixes. Any format-valid stream decodes, so
/// this only has to find good matches, not the same ones Prusa's encoder would.
struct Matcher<'a> {
    source: &'a [u8],
    params: Params,
    /// Latest position holding each hash, stored one greater so zero is empty.
    head: Vec<u32>,
    /// Previous position sharing a position's hash, stored the same way.
    previous: Vec<u32>,
    indexed: usize,
}

impl<'a> Matcher<'a> {
    fn new(source: &'a [u8], params: Params) -> Self {
        Self {
            source,
            params,
            head: vec![0; 1 << HASH_BITS],
            previous: vec![0; source.len() + 1],
            indexed: 0,
        }
    }

    fn longest(&mut self, at: usize) -> Option<(usize, usize)> {
        while self.indexed < at {
            self.index(self.indexed);
            self.indexed += 1;
        }

        let remaining = self.source.len() - at;
        let longest = (1usize << self.params.lookahead).min(remaining);
        if longest < MIN_MATCH {
            return None;
        }
        let furthest = 1usize << self.params.window;

        let mut best: Option<(usize, usize)> = None;
        let mut candidate = self.head[hash(&self.source[at..])];
        for _ in 0..CHAIN_LIMIT {
            let Some(start) = (candidate as usize).checked_sub(1) else {
                break;
            };
            candidate = self.previous[start];
            if at - start > furthest {
                break;
            }
            let length = self.run(start, at, longest);
            if length >= MIN_MATCH && best.is_none_or(|(_, best)| length > best) {
                best = Some((at - start, length));
                if length == longest {
                    break;
                }
            }
        }
        best
    }

    fn index(&mut self, at: usize) {
        if at + MIN_MATCH <= self.source.len() {
            let slot = hash(&self.source[at..]);
            self.previous[at] = self.head[slot];
            self.head[slot] = at as u32 + 1;
        }
    }

    fn run(&self, start: usize, at: usize, limit: usize) -> usize {
        (0..limit)
            .take_while(|offset| self.source[start + offset] == self.source[at + offset])
            .count()
    }
}

fn hash(bytes: &[u8]) -> usize {
    let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
    (value.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(data: &[u8], params: Params) {
        let encoded = encode(data, params);
        let decoded = decode(&encoded, params, data.len()).expect("stream ends early");
        assert_eq!(decoded, data);
    }

    #[test]
    fn round_trips_repetitive_gcode() {
        let mut data = String::new();
        for layer in 0..200 {
            data.push_str(&format!("G1 X10.5 Y20.5 Z{layer}.2 E1.5 F1200\n"));
        }
        round_trip(data.as_bytes(), Params::W12);
        round_trip(data.as_bytes(), Params::W11);
    }

    #[test]
    fn round_trips_incompressible_data() {
        let data: Vec<u8> = (0..4096)
            .map(|i| ((i * 2654435761u64) >> 13) as u8)
            .collect();
        round_trip(&data, Params::W12);
    }

    #[test]
    fn round_trips_edge_cases() {
        round_trip(b"", Params::W12);
        round_trip(b"a", Params::W12);
        round_trip(&[0u8; 5000], Params::W12);
        round_trip(&[0xFFu8; 33], Params::W11);
    }

    /// The bound an uncompressed length out of the file is checked against.
    /// One repeated byte is the best a back reference can do, so a stream of it
    /// is what would break a bound that is too tight; a length past the bound
    /// must be refused rather than reserved for, because reserving for it is
    /// what aborts the process.
    #[test]
    fn expansion_bounds_what_a_stream_can_produce() {
        for params in [Params::W11, Params::W12] {
            let data = vec![b'x'; 1 << 16];
            let encoded = encode(&data, params);
            assert!(
                data.len() <= encoded.len() * params.expansion(),
                "{params:?}: {} bytes from {}",
                data.len(),
                encoded.len()
            );
            assert_eq!(decode(&encoded, params, data.len()), Some(data));
            assert_eq!(decode(&encoded, params, usize::MAX), None, "{params:?}");
        }
    }

    #[test]
    fn compresses_repetition() {
        let data = "G1 X1 Y1\n".repeat(500);
        let encoded = encode(data.as_bytes(), Params::W12);
        assert!(
            encoded.len() < data.len() / 3,
            "expected compression, got {} from {}",
            encoded.len(),
            data.len()
        );
    }

    #[test]
    fn refuses_a_truncated_stream() {
        let encoded = encode(b"G1 X1 Y1 E1\n", Params::W12);
        assert_eq!(decode(&encoded[..1], Params::W12, 12), None);
    }
}
