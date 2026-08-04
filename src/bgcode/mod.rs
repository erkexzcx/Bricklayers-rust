//! Prusa's binary G-code container.
//!
//! A file is a ten byte header followed by blocks: metadata, thumbnails and
//! then the G-code itself. Only the G-code blocks are of interest here, so they
//! are decoded to text and every other block is kept exactly as it was read.
//! Rewriting therefore cannot disturb thumbnails, printer settings or the
//! slicer's own configuration, and the file keeps the compression it arrived
//! with.

mod heatshrink;
mod meatpack;

use std::io::{self, Read as _, Write};

use flate2::Compression as Level;
use flate2::read::{ZlibDecoder, ZlibEncoder};

const MAGIC: &[u8; 4] = b"GCDE";
const HEADER: usize = 10;
const VERSION: u32 = 1;
const GCODE_BLOCK: u16 = 1;

/// Uncompressed bytes per generated G-code block, matching the order of
/// magnitude a slicer emits so firmware still streams the file in small steps.
const BLOCK_TARGET: usize = 64 * 1024;

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Everything needed to put a rewritten G-code stream back into the file it
/// came from.
#[derive(Clone, Debug)]
pub struct Container {
    version: u32,
    checksums: bool,
    /// Every block that is not G-code, byte for byte as it was read.
    prelude: Vec<u8>,
    compression: Compression,
    /// Layer height from the file's metadata, which plain G-code carries as a
    /// comment but binary G-code keeps out of the G-code stream.
    pub layer_height: Option<f64>,
    /// First layer height from the same metadata.
    pub first_layer_height: Option<f64>,
}

impl Container {
    /// A sink that packs a G-code stream into blocks as it is written, so a
    /// rewrite never has to exist in memory all at once.
    pub fn writer<W: Write>(&self, mut out: W) -> io::Result<BlockWriter<W>> {
        out.write_all(MAGIC)?;
        out.write_all(&self.version.to_le_bytes())?;
        out.write_all(&u16::from(self.checksums).to_le_bytes())?;
        out.write_all(&self.prelude)?;
        Ok(BlockWriter {
            container: self.clone(),
            out,
            pending: Vec::with_capacity(BLOCK_TARGET * 2),
        })
    }

    pub fn serialize(&self, gcode: &str) -> Vec<u8> {
        let out = Vec::with_capacity(HEADER + self.prelude.len() + gcode.len() / 2);
        let mut writer = self.writer(out).expect("writing to a Vec cannot fail");
        writer
            .write_all(gcode.as_bytes())
            .expect("writing to a Vec cannot fail");
        writer.finish().expect("writing to a Vec cannot fail")
    }

    /// One complete G-code block, header and checksum included.
    fn block(&self, data: &[u8]) -> Vec<u8> {
        let payload = compress(data, self.compression);
        let mut block = Vec::with_capacity(payload.len() + 16);

        block.extend_from_slice(&GCODE_BLOCK.to_le_bytes());
        block.extend_from_slice(&self.compression.code().to_le_bytes());
        block.extend_from_slice(&(data.len() as u32).to_le_bytes());
        if self.compression != Compression::None {
            block.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        }
        // Encoding 0: the text is stored as-is rather than re-packed, because
        // MeatPack discards whitespace and inline comments.
        block.extend_from_slice(&0u16.to_le_bytes());
        block.extend_from_slice(&payload);

        if self.checksums {
            block.extend_from_slice(&crc32fast::hash(&block).to_le_bytes());
        }
        block
    }
}

/// Turns a G-code byte stream into container blocks on the fly.
///
/// Text is held only until it reaches a block boundary, so the writer needs a
/// couple of block sizes of memory no matter how long the stream runs.
pub struct BlockWriter<W: Write> {
    container: Container,
    out: W,
    pending: Vec<u8>,
}

impl<W: Write> BlockWriter<W> {
    /// Writes the tail of the stream as a final block and returns the sink.
    pub fn finish(mut self) -> io::Result<W> {
        if !self.pending.is_empty() {
            let block = self.container.block(&self.pending);
            self.out.write_all(&block)?;
            self.pending.clear();
        }
        self.out.flush()?;
        Ok(self.out)
    }

    /// Emits every whole block the pending text can supply, then compacts what
    /// is left over exactly once.
    fn drain(&mut self) -> io::Result<()> {
        let mut start = 0;
        while let Some(length) = block_end(&self.pending[start..]) {
            let block = self.container.block(&self.pending[start..start + length]);
            self.out.write_all(&block)?;
            start += length;
        }
        if start > 0 {
            self.pending.drain(..start);
        }
        Ok(())
    }
}

impl<W: Write> Write for BlockWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // Bounded so a single huge write cannot pull the whole stream into
        // memory; `write_all` comes back for the rest.
        let room = (BLOCK_TARGET * 2)
            .saturating_sub(self.pending.len())
            .max(BLOCK_TARGET);
        let taken = data.len().min(room);
        self.pending.extend_from_slice(&data[..taken]);
        self.drain()?;
        Ok(taken)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Length of the leading block in `pending`, or `None` while it is still too
/// short. A block ends at the first line boundary at or past the target size,
/// so no command is ever cut in half.
fn block_end(pending: &[u8]) -> Option<usize> {
    let earliest = BLOCK_TARGET.checked_sub(1)?;
    let at = pending
        .get(earliest..)?
        .iter()
        .position(|byte| *byte == b'\n')?;
    Some(earliest + at + 1)
}

pub fn parse(bytes: &[u8]) -> Result<(Container, String), String> {
    if !is_binary(bytes) || bytes.len() < HEADER {
        return Err("missing GCDE magic number".into());
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version > VERSION {
        return Err(format!("version {version} is newer than {VERSION}"));
    }
    let checksums = match u16::from_le_bytes(bytes[8..10].try_into().unwrap()) {
        0 => false,
        1 => true,
        other => return Err(format!("unknown checksum type {other}")),
    };

    let mut at = HEADER;
    let mut prelude = Vec::new();
    let mut gcode = String::new();
    let mut compression = Compression::Heatshrink12;
    let mut layer_height = None;
    let mut first_layer_height = None;

    while at < bytes.len() {
        let start = at;
        let kind = take_u16(bytes, &mut at)?;
        let code = take_u16(bytes, &mut at)?;
        let uncompressed = take_u32(bytes, &mut at)? as usize;
        let packing = Compression::from_code(code)
            .ok_or_else(|| format!("unknown compression type {code}"))?;
        let stored = if packing == Compression::None {
            uncompressed
        } else {
            take_u32(bytes, &mut at)? as usize
        };

        let parameters = take(bytes, &mut at, parameters_size(kind)?)?;
        let data = take(bytes, &mut at, stored)?;

        if checksums {
            let expected = u32::from_le_bytes(take(bytes, &mut at, 4)?.try_into().unwrap());
            let found = crc32fast::hash(&bytes[start..at - 4]);
            if found != expected {
                return Err(format!("checksum mismatch in the block at byte {start}"));
            }
        }

        if kind == GCODE_BLOCK {
            compression = packing;
            let raw = decompress(data, packing, uncompressed)?;
            let encoding = u16::from_le_bytes(parameters[..2].try_into().unwrap());
            let text = match encoding {
                0 => raw,
                1 | 2 => meatpack::decode(&raw),
                other => return Err(format!("unknown G-code encoding {other}")),
            };
            gcode.push_str(&String::from_utf8_lossy(&text));
        } else {
            prelude.extend_from_slice(&bytes[start..at]);
            if (layer_height.is_none() || first_layer_height.is_none())
                && let Ok(raw) = decompress(data, packing, uncompressed)
            {
                layer_height = layer_height.or_else(|| setting(&raw, "layer_height"));
                first_layer_height =
                    first_layer_height.or_else(|| setting(&raw, "first_layer_height"));
            }
        }
    }

    Ok((
        Container {
            version,
            checksums,
            prelude,
            compression,
            layer_height,
            first_layer_height,
        },
        gcode,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compression {
    None,
    Deflate,
    Heatshrink11,
    Heatshrink12,
}

impl Compression {
    fn from_code(code: u16) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::Deflate),
            2 => Some(Self::Heatshrink11),
            3 => Some(Self::Heatshrink12),
            _ => None,
        }
    }

    fn code(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Deflate => 1,
            Self::Heatshrink11 => 2,
            Self::Heatshrink12 => 3,
        }
    }
}

fn decompress(data: &[u8], packing: Compression, expected: usize) -> Result<Vec<u8>, String> {
    let out = match packing {
        Compression::None => data.to_vec(),
        Compression::Deflate => {
            let mut out = Vec::with_capacity(expected);
            ZlibDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|error| format!("deflate stream: {error}"))?;
            out
        }
        Compression::Heatshrink11 => heatshrink::decode(data, heatshrink::Params::W11, expected)
            .ok_or("heatshrink stream ends early")?,
        Compression::Heatshrink12 => heatshrink::decode(data, heatshrink::Params::W12, expected)
            .ok_or("heatshrink stream ends early")?,
    };
    if out.len() != expected {
        return Err(format!(
            "block holds {} bytes but its header promised {expected}",
            out.len()
        ));
    }
    Ok(out)
}

fn compress(data: &[u8], packing: Compression) -> Vec<u8> {
    match packing {
        Compression::None => data.to_vec(),
        Compression::Deflate => {
            let mut out = Vec::new();
            ZlibEncoder::new(data, Level::best())
                .read_to_end(&mut out)
                .expect("deflating a slice cannot fail");
            out
        }
        Compression::Heatshrink11 => heatshrink::encode(data, heatshrink::Params::W11),
        Compression::Heatshrink12 => heatshrink::encode(data, heatshrink::Params::W12),
    }
}

fn parameters_size(kind: u16) -> Result<usize, String> {
    match kind {
        // Metadata and G-code blocks name an encoding.
        0..=4 => Ok(2),
        // Thumbnails name a format and their dimensions.
        5 => Ok(6),
        other => Err(format!("unknown block type {other}")),
    }
}

/// Reads one `key=value` line out of a metadata block. Both keys this is asked
/// for are heights the nozzle is driven by, so a value that is not a length is
/// no better than a missing one.
fn setting(ini: &[u8], key: &str) -> Option<f64> {
    std::str::from_utf8(ini)
        .ok()?
        .lines()
        .find_map(|line| {
            let (found, value) = line.split_once('=')?;
            (found.trim() == key)
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .filter(crate::scan::is_a_height)
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, count: usize) -> Result<&'a [u8], String> {
    let end = at.checked_add(count).ok_or("block size overflows")?;
    let slice = bytes
        .get(*at..end)
        .ok_or_else(|| format!("file ends inside the block at byte {at}"))?;
    *at = end;
    Ok(slice)
}

fn take_u16(bytes: &[u8], at: &mut usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(take(bytes, at, 2)?.try_into().unwrap()))
}

fn take_u32(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(take(bytes, at, 4)?.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container() -> Container {
        Container {
            version: VERSION,
            checksums: true,
            prelude: Vec::new(),
            compression: Compression::Heatshrink12,
            layer_height: None,
            first_layer_height: None,
        }
    }

    /// Walks a serialized file and counts its G-code blocks.
    fn gcode_blocks(bytes: &[u8]) -> usize {
        let checksums = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) == 1;
        let mut at = HEADER;
        let mut blocks = 0;

        while at < bytes.len() {
            let kind = take_u16(bytes, &mut at).expect("block type");
            let code = take_u16(bytes, &mut at).expect("compression");
            let uncompressed = take_u32(bytes, &mut at).expect("size") as usize;
            let packing = Compression::from_code(code).expect("known compression");
            let stored = if packing == Compression::None {
                uncompressed
            } else {
                take_u32(bytes, &mut at).expect("stored size") as usize
            };
            take(bytes, &mut at, parameters_size(kind).expect("parameters")).expect("parameters");
            take(bytes, &mut at, stored).expect("payload");
            if checksums {
                take(bytes, &mut at, 4).expect("checksum");
            }
            blocks += usize::from(kind == GCODE_BLOCK);
        }
        blocks
    }

    #[test]
    fn rejects_plain_text() {
        assert!(!is_binary(b"G1 X1 Y1\n"));
        assert!(parse(b"G1 X1 Y1\n").is_err());
    }

    #[test]
    fn round_trips_through_the_container() {
        let gcode = "; header\nG1 X1 Y1 E1\n;TYPE:Perimeter\nG1 X2 Y2 E2\n".repeat(400);
        let bytes = container().serialize(&gcode);

        assert!(is_binary(&bytes));
        let (parsed, text) = parse(&bytes).expect("round trip should parse");
        assert_eq!(text, gcode);
        assert_eq!(parsed.compression, Compression::Heatshrink12);
    }

    #[test]
    fn round_trips_under_every_compression() {
        let gcode = "G1 X1 Y1 E1\n".repeat(9000);
        for packing in [
            Compression::None,
            Compression::Deflate,
            Compression::Heatshrink11,
            Compression::Heatshrink12,
        ] {
            let mut source = container();
            source.compression = packing;
            let bytes = source.serialize(&gcode);
            let (_, text) = parse(&bytes).unwrap_or_else(|error| panic!("{packing:?}: {error}"));
            assert_eq!(text, gcode, "{packing:?}");
        }
    }

    #[test]
    fn round_trips_without_checksums() {
        let mut source = container();
        source.checksums = false;
        let bytes = source.serialize("G1 X1\n");
        assert_eq!(parse(&bytes).unwrap().1, "G1 X1\n");
    }

    #[test]
    fn a_corrupted_byte_fails_the_checksum() {
        let mut bytes = container().serialize("G1 X1 Y1 E1\n");
        *bytes.last_mut().unwrap() ^= 0xFF;
        assert!(parse(&bytes).unwrap_err().contains("checksum"));
    }

    #[test]
    fn a_block_ends_at_the_first_line_break_past_the_target() {
        let mut data = vec![b'x'; BLOCK_TARGET * 2];
        data[BLOCK_TARGET - 2] = b'\n';
        data[BLOCK_TARGET + 5] = b'\n';
        assert_eq!(block_end(&data), Some(BLOCK_TARGET + 6));

        // Too short to close a block, and long enough but with nowhere to cut.
        assert_eq!(block_end(b"G1 X1\n"), None);
        assert_eq!(block_end(&vec![b'x'; BLOCK_TARGET * 2]), None);
    }

    #[test]
    fn long_gcode_is_split_across_blocks() {
        let gcode = "G1 X123.456 Y234.567 E1.234\n".repeat(20_000);
        let bytes = container().serialize(&gcode);

        assert!(gcode_blocks(&bytes) > 1, "expected several blocks");
        assert_eq!(parse(&bytes).expect("round trip").1, gcode);
    }

    #[test]
    fn a_trailing_line_without_a_newline_survives() {
        let bytes = container().serialize("G1 X1\nG1 X2");
        assert_eq!(parse(&bytes).expect("round trip").1, "G1 X1\nG1 X2");
        assert_eq!(gcode_blocks(&bytes), 1);
    }

    #[test]
    fn empty_gcode_writes_no_blocks() {
        let bytes = container().serialize("");
        assert_eq!(gcode_blocks(&bytes), 0);
        assert_eq!(parse(&bytes).expect("round trip").1, "");
    }

    /// The block layout has to depend on the text alone, not on how the
    /// transform happened to hand it over.
    #[test]
    fn chunked_writes_produce_an_identical_file() {
        let gcode = "; a comment\nG1 X1 Y1 E1\n".repeat(8_000);
        let whole = container().serialize(&gcode);

        for chunk in [1, 7, 4096, BLOCK_TARGET, BLOCK_TARGET * 3] {
            let mut writer = container().writer(Vec::new()).expect("start writer");
            for piece in gcode.as_bytes().chunks(chunk) {
                writer.write_all(piece).expect("write piece");
            }
            assert_eq!(
                writer.finish().expect("finish"),
                whole,
                "chunk size {chunk}"
            );
        }
    }

    #[test]
    fn reads_a_metadata_setting() {
        assert_eq!(
            setting(b"a=1\nlayer_height=0.25\nz=9\n", "layer_height"),
            Some(0.25)
        );
        assert_eq!(setting(b"first_layer_height=0.3\n", "layer_height"), None);
    }

    /// Both keys are heights the nozzle is driven by, so a metadata block that
    /// states one that is not a length must read as if it had said nothing.
    #[test]
    fn a_metadata_height_that_is_not_a_length_is_ignored() {
        for value in ["0", "-0.2", "nan", "inf", "thick", ""] {
            let ini = format!("layer_height={value}\n");
            assert_eq!(setting(ini.as_bytes(), "layer_height"), None, "{value}");
        }
    }
}
