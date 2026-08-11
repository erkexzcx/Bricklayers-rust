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

use std::io::{self, BufRead, Cursor, Read, Seek, SeekFrom, Write};

use flate2::Compression as Level;
use flate2::read::{ZlibDecoder, ZlibEncoder};

const MAGIC: &[u8; 4] = b"GCDE";
const HEADER: usize = 10;
const VERSION: u32 = 1;
const GCODE_BLOCK: u16 = 1;
/// A block's trailing CRC32, when the file carries checksums at all.
const CHECKSUM: usize = 4;

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
    /// Width the internal perimeters were metered at, in mm, from the same
    /// metadata, resolved against the nozzle where it is stated as a share of
    /// it.
    pub wall_width: Option<f64>,
    /// Nozzle diameter in mm.
    pub nozzle: Option<f64>,
}

impl Container {
    /// Walks a container's block chain, keeping only what a rewrite has to put
    /// back: the blocks that are not G-code, byte for byte, and the layer
    /// height a binary file states outside its G-code stream.
    ///
    /// G-code payloads are stepped over rather than unpacked, so this costs a
    /// walk over the block headers and no memory that grows with the print.
    /// Their checksums are therefore left to [`Container::blocks`], which is
    /// still a pass earlier than the first byte of output.
    pub fn read<R: BufRead + Seek>(mut input: R) -> Result<Self, String> {
        let length = input
            .seek(SeekFrom::End(0))
            .and_then(|length| input.rewind().map(|()| length))
            .map_err(|error| format!("cannot be read: {error}"))?;
        let (version, checksums) = file_header(&mut input)?;

        let trailer = if checksums { CHECKSUM } else { 0 };
        let mut at = HEADER as u64;
        let mut prelude = Vec::new();
        let mut block = Vec::new();
        let mut compression = Compression::Heatshrink12;
        let mut layer_height = None;
        let mut wall_width: Option<String> = None;
        let mut nozzle: Option<String> = None;

        while !ended(&mut input)? {
            let start = at;
            block.clear();
            let header = read_header(&mut input, &mut block, start)?;
            at += block.len() as u64;
            let rest = header.stored + trailer;

            if header.kind == GCODE_BLOCK {
                compression = header.packing;
                if at + rest as u64 > length {
                    return Err(format!("file ends inside the block at byte {start}"));
                }
                input
                    .seek(SeekFrom::Current(rest as i64))
                    .map_err(|error| format!("cannot be read: {error}"))?;
                at += rest as u64;
                continue;
            }

            take(&mut input, &mut block, rest, start)?;
            at += rest as u64;
            verify(&block, checksums, start)?;

            if (layer_height.is_none() || wall_width.is_none() || nozzle.is_none())
                && let Ok(ini) = decompress(
                    payload(&block, &header, trailer),
                    header.packing,
                    header.uncompressed,
                )
            {
                if layer_height.is_none() {
                    layer_height = setting(&ini, "layer_height");
                }
                if wall_width.is_none() {
                    wall_width = text(&ini, "perimeter_extrusion_width")
                        .or_else(|| text(&ini, "inner_wall_line_width"));
                }
                if nozzle.is_none() {
                    nozzle = text(&ini, "nozzle_diameter");
                }
            }
            prelude.extend_from_slice(&block);
        }

        let nozzle = crate::scan::width(nozzle.as_deref(), None);
        Ok(Self {
            version,
            checksums,
            prelude,
            compression,
            layer_height,
            wall_width: crate::scan::width(wall_width.as_deref(), nozzle),
            nozzle,
        })
    }

    /// A reader over the file's G-code, one block at a time.
    pub fn blocks<R: BufRead>(&self, mut input: R) -> Result<BlockReader<R>, String> {
        file_header(&mut input)?;
        Ok(BlockReader {
            input,
            checksums: self.checksums,
            at: HEADER as u64,
            block: Vec::new(),
            text: Vec::new(),
            read: 0,
        })
    }

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
    let container = Container::read(Cursor::new(bytes))?;
    let mut gcode = Vec::new();
    container
        .blocks(Cursor::new(bytes))?
        .read_to_end(&mut gcode)
        .map_err(|error| error.to_string())?;
    Ok((container, String::from_utf8_lossy(&gcode).into_owned()))
}

/// Decodes a container's G-code blocks one at a time.
///
/// Only the block being read is unpacked, so a pass over a print's worth of
/// G-code costs one block of memory however long the print is. Blocks that are
/// not G-code are stepped over: a rewrite puts back the copies
/// [`Container::read`] kept.
pub struct BlockReader<R: BufRead> {
    input: R,
    checksums: bool,
    /// Byte offset of the next block, for error messages only.
    at: u64,
    /// Scratch holding the current block as it was stored.
    block: Vec<u8>,
    /// The current block, decoded.
    text: Vec<u8>,
    read: usize,
}

impl<R: BufRead> BlockReader<R> {
    /// Decodes the next G-code block, skipping anything else. `false` once the
    /// chain has ended.
    fn next_block(&mut self) -> Result<bool, String> {
        let trailer = if self.checksums { CHECKSUM } else { 0 };

        while !ended(&mut self.input)? {
            let start = self.at;
            self.block.clear();
            let header = read_header(&mut self.input, &mut self.block, start)?;
            self.at += self.block.len() as u64;
            let rest = header.stored + trailer;

            if header.kind != GCODE_BLOCK {
                discard(&mut self.input, rest, start)?;
                self.at += rest as u64;
                continue;
            }

            take(&mut self.input, &mut self.block, rest, start)?;
            self.at += rest as u64;
            verify(&self.block, self.checksums, start)?;

            let stored = payload(&self.block, &header, trailer);
            let raw = decompress(stored, header.packing, header.uncompressed)?;
            self.text = match header.encoding {
                0 => raw,
                1 | 2 => meatpack::decode(&raw),
                other => return Err(format!("unknown G-code encoding {other}")),
            };
            self.read = 0;
            return Ok(true);
        }
        Ok(false)
    }
}

impl<R: BufRead> BufRead for BlockReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        while self.read == self.text.len() {
            if !self.next_block().map_err(damaged)? {
                break;
            }
        }
        Ok(&self.text[self.read..])
    }

    fn consume(&mut self, amount: usize) {
        self.read = self.read.saturating_add(amount).min(self.text.len());
    }
}

impl<R: BufRead> Read for BlockReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let taken = available.len().min(buffer.len());
        buffer[..taken].copy_from_slice(&available[..taken]);
        self.consume(taken);
        Ok(taken)
    }
}

/// A block that will not decode surfaces mid-pass rather than when the file was
/// opened, so it has to carry its own description of what went wrong.
fn damaged(reason: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("not valid binary G-code: {reason}"),
    )
}

/// A block's fixed part: everything before its payload.
struct Header {
    kind: u16,
    packing: Compression,
    uncompressed: usize,
    stored: usize,
    encoding: u16,
}

/// The ten byte file header. `block` must be empty.
fn file_header<R: Read>(input: &mut R) -> Result<(u32, bool), String> {
    let mut head = [0u8; HEADER];
    input
        .read_exact(&mut head)
        .map_err(|_| "missing GCDE magic number".to_string())?;
    if !is_binary(&head) {
        return Err("missing GCDE magic number".into());
    }
    let version = u32::from_le_bytes(head[4..8].try_into().unwrap());
    if version > VERSION {
        return Err(format!("version {version} is newer than {VERSION}"));
    }
    let checksums = match u16::from_le_bytes(head[8..10].try_into().unwrap()) {
        0 => false,
        1 => true,
        other => return Err(format!("unknown checksum type {other}")),
    };
    Ok((version, checksums))
}

/// Reads one block's header into `block`, which must be empty.
fn read_header<R: Read>(input: &mut R, block: &mut Vec<u8>, at: u64) -> Result<Header, String> {
    take(input, block, 4, at)?;
    let kind = u16::from_le_bytes(block[0..2].try_into().unwrap());
    let code = u16::from_le_bytes(block[2..4].try_into().unwrap());
    let packing =
        Compression::from_code(code).ok_or_else(|| format!("unknown compression type {code}"))?;

    let sizes = if packing == Compression::None { 4 } else { 8 };
    take(input, block, sizes + parameters_size(kind)?, at)?;
    let uncompressed = u32::from_le_bytes(block[4..8].try_into().unwrap()) as usize;
    let stored = if packing == Compression::None {
        uncompressed
    } else {
        u32::from_le_bytes(block[8..12].try_into().unwrap()) as usize
    };
    let encoding = u16::from_le_bytes(block[4 + sizes..6 + sizes].try_into().unwrap());

    Ok(Header {
        kind,
        packing,
        uncompressed,
        stored,
        encoding,
    })
}

/// The stored payload of a complete block, between its header and its checksum.
fn payload<'a>(block: &'a [u8], header: &Header, trailer: usize) -> &'a [u8] {
    let end = block.len() - trailer;
    &block[end - header.stored..end]
}

fn verify(block: &[u8], checksums: bool, at: u64) -> Result<(), String> {
    if !checksums {
        return Ok(());
    }
    let split = block.len() - CHECKSUM;
    let expected = u32::from_le_bytes(block[split..].try_into().unwrap());
    if crc32fast::hash(&block[..split]) != expected {
        return Err(format!("checksum mismatch in the block at byte {at}"));
    }
    Ok(())
}

/// Whether the chain has ended exactly on a block boundary.
fn ended<R: BufRead>(input: &mut R) -> Result<bool, String> {
    input
        .fill_buf()
        .map(<[u8]>::is_empty)
        .map_err(|error| format!("cannot be read: {error}"))
}

/// Appends exactly `count` bytes to `block`.
///
/// The count comes out of the file, so it is never reserved for up front: a
/// corrupt length has to fail as a short read, not as a huge allocation.
fn take<R: Read>(input: &mut R, block: &mut Vec<u8>, count: usize, at: u64) -> Result<(), String> {
    let read = input
        .by_ref()
        .take(count as u64)
        .read_to_end(block)
        .map_err(|error| format!("cannot be read: {error}"))?;
    if read != count {
        return Err(format!("file ends inside the block at byte {at}"));
    }
    Ok(())
}

/// Steps over a block this pass has no use for.
fn discard<R: Read>(input: &mut R, count: usize, at: u64) -> Result<(), String> {
    let skipped = io::copy(&mut input.by_ref().take(count as u64), &mut io::sink())
        .map_err(|error| format!("cannot be read: {error}"))?;
    if skipped != count as u64 {
        return Err(format!("file ends inside the block at byte {at}"));
    }
    Ok(())
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

/// Reads one `key=value` line out of a metadata block. The key this is asked
/// for is a height the nozzle is driven by, so a value that is not a length is
/// no better than a missing one.
fn setting(ini: &[u8], key: &str) -> Option<f64> {
    text(ini, key)?
        .parse()
        .ok()
        .filter(crate::scan::is_a_height)
}

/// The value of one `key=value` line, exactly as the metadata block states it.
/// A width is only a number once the nozzle it may be a percentage of has been
/// read, which can be a block later.
fn text(ini: &[u8], key: &str) -> Option<String> {
    std::str::from_utf8(ini).ok()?.lines().find_map(|line| {
        let (found, value) = line.split_once('=')?;
        (found.trim() == key).then(|| value.trim().to_owned())
    })
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
            wall_width: None,
            nozzle: None,
        }
    }

    /// Walks a serialized file and counts its G-code blocks.
    fn gcode_blocks(bytes: &[u8]) -> usize {
        let mut input = Cursor::new(bytes);
        let (_, checksums) = file_header(&mut input).expect("file header");
        let trailer = if checksums { CHECKSUM } else { 0 };
        let mut block = Vec::new();
        let mut blocks = 0;

        while !ended(&mut input).expect("read") {
            block.clear();
            let header = read_header(&mut input, &mut block, 0).expect("block header");
            discard(&mut input, header.stored + trailer, 0).expect("payload");
            blocks += usize::from(header.kind == GCODE_BLOCK);
        }
        blocks
    }

    /// A container built by hand, so a block can hold exactly these bytes
    /// rather than whatever the writer's own boundaries would produce.
    fn packed(container: &Container, chunks: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&container.version.to_le_bytes());
        bytes.extend_from_slice(&u16::from(container.checksums).to_le_bytes());
        bytes.extend_from_slice(&container.prelude);
        for chunk in chunks {
            bytes.extend_from_slice(&container.block(chunk));
        }
        bytes
    }

    /// One metadata block, the shape a slicer writes its settings in.
    fn metadata(container: &Container, kind: u16, ini: &str) -> Vec<u8> {
        let payload = compress(ini.as_bytes(), container.compression);
        let mut block = Vec::new();
        block.extend_from_slice(&kind.to_le_bytes());
        block.extend_from_slice(&container.compression.code().to_le_bytes());
        block.extend_from_slice(&(ini.len() as u32).to_le_bytes());
        block.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        block.extend_from_slice(&0u16.to_le_bytes());
        block.extend_from_slice(&payload);
        if container.checksums {
            block.extend_from_slice(&crc32fast::hash(&block).to_le_bytes());
        }
        block
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

    /// The whole point of the block reader: a file's G-code is decoded a block
    /// at a time, so what is held never grows with the print.
    #[test]
    fn only_one_block_is_unpacked_at_a_time() {
        let gcode = "G1 X123.456 Y234.567 E1.234\n".repeat(40_000);
        let bytes = container().serialize(&gcode);
        assert!(gcode_blocks(&bytes) > 10, "expected many blocks");

        let mut reader = container()
            .blocks(Cursor::new(&bytes[..]))
            .expect("start reader");
        let mut read = Vec::new();
        let mut buffer = [0u8; 1024];

        loop {
            let taken = reader.read(&mut buffer).expect("read");
            if taken == 0 {
                break;
            }
            read.extend_from_slice(&buffer[..taken]);
            assert!(
                reader.text.len() <= BLOCK_TARGET * 2,
                "{} bytes unpacked after {} read",
                reader.text.len(),
                read.len()
            );
        }
        assert_eq!(read, gcode.as_bytes());
    }

    /// Blocks written elsewhere need not end on a line, and a reader that
    /// returned bytes a block at a time would cut such a line in half.
    #[test]
    fn a_line_split_across_two_blocks_is_read_whole() {
        let bytes = packed(&container(), &[b"G1 X1 Y1 ", b"E1\nG1 X2\n"]);
        assert_eq!(gcode_blocks(&bytes), 2);

        let mut lines = Vec::new();
        let reader = container().blocks(Cursor::new(&bytes[..])).expect("reader");
        for line in reader.lines() {
            lines.push(line.expect("line"));
        }
        assert_eq!(lines, ["G1 X1 Y1 E1", "G1 X2"]);
    }

    /// Reading in awkward steps must not lose or repeat a byte at a block
    /// boundary, whatever size the caller asks for.
    #[test]
    fn any_read_size_returns_the_same_stream() {
        let gcode = "; a comment\nG1 X1 Y1 E1\n".repeat(8_000);
        let bytes = container().serialize(&gcode);

        for size in [1, 3, 4096, BLOCK_TARGET - 1, BLOCK_TARGET * 3] {
            let mut reader = container().blocks(Cursor::new(&bytes[..])).expect("reader");
            let mut read = Vec::new();
            let mut buffer = vec![0u8; size];
            loop {
                let taken = reader.read(&mut buffer).expect("read");
                if taken == 0 {
                    break;
                }
                read.extend_from_slice(&buffer[..taken]);
            }
            assert_eq!(read, gcode.as_bytes(), "read size {size}");
        }
    }

    /// Opening a file walks its block headers without unpacking any G-code, so
    /// a G-code block that will not decode has to survive that walk and fail on
    /// the pass that reads it — which is still before a byte of output.
    #[test]
    fn opening_a_file_does_not_decode_its_gcode() {
        let sound = packed(&container(), &[b"G1 X1 Y1 E1\n"]);

        // A size only a decode compares against what it unpacked. The stored
        // length is untouched, so the walk still steps over exactly the block.
        let mut lying = sound.clone();
        lying[HEADER + 4..HEADER + 8].copy_from_slice(&99u32.to_le_bytes());
        let repaired = crc32fast::hash(&lying[HEADER..lying.len() - CHECKSUM]);
        let end = lying.len() - CHECKSUM;
        lying[end..].copy_from_slice(&repaired.to_le_bytes());

        Container::read(Cursor::new(&lying[..])).expect("the walk should not decode");
        assert!(
            parse(&lying).is_err(),
            "a block that lies should not decode"
        );

        // And a G-code block's checksum, which the walk steps over with it.
        let mut corrupt = sound.clone();
        *corrupt.last_mut().unwrap() ^= 0xFF;

        Container::read(Cursor::new(&corrupt[..])).expect("the walk should not checksum G-code");
        assert!(parse(&corrupt).expect_err("corrupt").contains("checksum"));
    }

    /// A rewrite puts the prelude back before the G-code it packs, so a
    /// metadata block that trailed the G-code must still be found and kept.
    #[test]
    fn metadata_is_found_wherever_it_sits() {
        let source = container();
        let mut bytes = packed(&source, &[b"G1 X1 Y1 E1\n"]);
        let trailing = metadata(&source, 4, "layer_height=0.25\nfirst_layer_height=0.3\n");
        bytes.extend_from_slice(&trailing);

        let container = Container::read(Cursor::new(&bytes[..])).expect("walk the chain");
        assert_eq!(container.layer_height, Some(0.25));
        assert_eq!(container.prelude, trailing, "kept byte for byte");
        assert_eq!(parse(&bytes).expect("decode").1, "G1 X1 Y1 E1\n");
    }

    /// A truncated file is refused when it is opened, before a transform has a
    /// chance to write anything.
    #[test]
    fn a_file_that_ends_inside_a_block_is_refused() {
        let bytes = container().serialize(&"G1 X1 Y1 E1\n".repeat(200));
        for cut in [HEADER + 3, HEADER + 12, bytes.len() - 1] {
            let error = Container::read(Cursor::new(&bytes[..cut]))
                .expect_err("a short file should not parse");
            assert!(error.contains("ends inside"), "{cut}: {error}");
        }
    }

    /// A length taken from the file must fail as a short read rather than be
    /// reserved for, or a corrupt header becomes a four gigabyte allocation.
    #[test]
    fn an_impossible_block_length_is_not_allocated_for() {
        let mut bytes = container().serialize("G1 X1 Y1 E1\n");
        bytes[HEADER + 8..HEADER + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            Container::read(Cursor::new(&bytes[..]))
                .expect_err("an impossible length should not parse")
                .contains("ends inside")
        );
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
