//! Post-processes sliced G-code so that layers interlock instead of stacking
//! as independent flat sheets, which is where FDM prints are weakest.
//!
//! [`brick`] raises every other internal perimeter loop by half a layer
//! height, staggering the seams between loops.
//!
//! It streams: G-code arrives a line at a time from a [`Source`] and leaves a
//! line at a time through a [`Sink`], so a file is never held in memory.

pub mod bgcode;
pub mod brick;
mod error;
pub mod feature;
pub mod gcode;
pub mod scan;
pub mod slicer;

pub use error::{Error, Result};

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use crate::scan::Survey;

/// Bytes buffered before touching the disk. The transform writes one short line
/// at a time, so this is what keeps that from becoming one syscall per line.
const WRITE_BUFFER: usize = 256 * 1024;

/// A G-code file: where to read it from, and which container a rewrite has to
/// go back into.
///
/// The transform needs a [`Survey`] of the whole file before it can rewrite any
/// of it, so the input is read twice. Plain text is re-read from the disk
/// and never held in memory. Binary G-code is decoded once and kept, because
/// its blocks have to be unpacked before a single line can be read; that costs
/// the size of the decoded G-code and nothing more, since the rewrite still
/// streams back out.
#[derive(Clone, Debug)]
pub struct Source {
    path: PathBuf,
    kind: Kind,
}

#[derive(Clone, Debug)]
enum Kind {
    Text,
    Binary {
        container: bgcode::Container,
        gcode: String,
    },
}

impl Source {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(|source| Error::io(path, source))?;
        let mut magic = [0u8; 4];
        let read = fill(&mut file, &mut magic).map_err(|source| Error::io(path, source))?;

        let path = path.to_path_buf();
        if !bgcode::is_binary(&magic[..read]) {
            return Ok(Self {
                path,
                kind: Kind::Text,
            });
        }

        let mut bytes = magic[..read].to_vec();
        file.read_to_end(&mut bytes)
            .map_err(|source| Error::io(&path, source))?;
        let (container, gcode) = bgcode::parse(&bytes).map_err(|reason| Error::Bgcode {
            path: path.clone(),
            reason,
        })?;

        Ok(Self {
            path,
            kind: Kind::Binary { container, gcode },
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_binary(&self) -> bool {
        matches!(self.kind, Kind::Binary { .. })
    }

    /// Layer height the file states about itself. Plain G-code carries it as a
    /// comment the survey already sees; binary G-code keeps it in a metadata
    /// block, outside the G-code stream.
    pub fn layer_height(&self) -> Option<f64> {
        match &self.kind {
            Kind::Text => None,
            Kind::Binary { container, .. } => container.layer_height,
        }
    }

    /// First layer height the file states about itself, from a binary
    /// container's metadata. A plain file's comment is left to the survey.
    pub fn first_layer_height(&self) -> Option<f64> {
        match &self.kind {
            Kind::Text => None,
            Kind::Binary { container, .. } => container.first_layer_height,
        }
    }

    /// The decoded G-code, for a binary container only.
    pub fn decoded(&self) -> Option<&str> {
        match &self.kind {
            Kind::Text => None,
            Kind::Binary { gcode, .. } => Some(gcode),
        }
    }

    /// A reader positioned at the start. Each call is an independent pass.
    pub fn reader(&self) -> Result<Reader<'_>> {
        Ok(Reader(match &self.kind {
            Kind::Text => {
                let file = File::open(&self.path).map_err(|s| Error::io(&self.path, s))?;
                Stream::File(BufReader::with_capacity(WRITE_BUFFER, file))
            }
            Kind::Binary { gcode, .. } => Stream::Memory(Cursor::new(gcode.as_bytes())),
        }))
    }

    pub fn survey(&self) -> Result<Survey> {
        Survey::read(self.reader()?).map_err(|source| Error::io(&self.path, source))
    }

    /// A destination that will replace `target` with this file's container
    /// format once the rewrite is complete.
    pub fn sink(&self, target: &Path) -> Result<Sink> {
        let temporary = temporary_path(target);
        // Named for the target, since that is the path the caller asked for
        // and the temporary beside it is this module's business.
        let file = File::create(&temporary).map_err(|source| Error::io(target, source))?;
        inherit_mode(&file, [target, &self.path]);

        let writer = match &self.kind {
            Kind::Text => Writer::Text(BufWriter::with_capacity(WRITE_BUFFER, file)),
            Kind::Binary { container, .. } => Writer::Binary(
                container
                    .writer(file)
                    .map_err(|source| Error::io(&temporary, source))?,
            ),
        };

        Ok(Sink {
            writer: Some(writer),
            temporary,
            target: target.to_path_buf(),
            committed: false,
        })
    }

    /// Streams this source through `transform` and commits the result.
    ///
    /// The reader is closed before the sink replaces its target, so rewriting a
    /// file in place never renames over a handle that is still open.
    pub fn rewrite<T>(
        &self,
        mut sink: Sink,
        transform: impl FnOnce(Reader<'_>, &mut Sink) -> io::Result<T>,
    ) -> Result<T> {
        let outcome = {
            let reader = self.reader()?;
            transform(reader, &mut sink).map_err(|source| Error::io(&self.path, source))?
        };
        sink.commit()?;
        Ok(outcome)
    }
}

/// One pass over a [`Source`].
pub struct Reader<'a>(Stream<'a>);

enum Stream<'a> {
    File(BufReader<File>),
    Memory(Cursor<&'a [u8]>),
}

impl Read for Reader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            Stream::File(reader) => reader.read(buffer),
            Stream::Memory(reader) => reader.read(buffer),
        }
    }
}

impl BufRead for Reader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match &mut self.0 {
            Stream::File(reader) => reader.fill_buf(),
            Stream::Memory(reader) => reader.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match &mut self.0 {
            Stream::File(reader) => reader.consume(amount),
            Stream::Memory(reader) => reader.consume(amount),
        }
    }
}

/// A write destination that only replaces its target once the rewrite is
/// complete.
///
/// Slicers hand a post-processor the one copy of the G-code they have, so a
/// crash partway through must leave the original untouched. Everything goes to
/// a temporary file beside the target, which is renamed over it at the end and
/// deleted if the rewrite never gets there.
pub struct Sink {
    writer: Option<Writer>,
    temporary: PathBuf,
    target: PathBuf,
    committed: bool,
}

enum Writer {
    Text(BufWriter<File>),
    Binary(bgcode::BlockWriter<File>),
}

impl Sink {
    /// Finishes the file and moves it over the target.
    pub fn commit(mut self) -> Result<()> {
        let writer = self.writer.take().expect("a sink is committed once");
        writer
            .finish()
            .map_err(|source| Error::io(&self.temporary, source))?;
        fs::rename(&self.temporary, &self.target)
            .map_err(|source| Error::io(&self.target, source))?;
        self.committed = true;
        Ok(())
    }

    fn writer(&mut self) -> &mut Writer {
        self.writer.as_mut().expect("a sink is committed once")
    }
}

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.writer().write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer().flush()
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        if !self.committed {
            drop(self.writer.take());
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

impl Writer {
    fn finish(self) -> io::Result<()> {
        let file = match self {
            Writer::Text(out) => out.into_inner().map_err(io::IntoInnerError::into_error)?,
            Writer::Binary(out) => out.finish()?,
        };
        // The rename that follows is only atomic if the bytes reached the disk
        // first. Without this a power cut can leave the target's name pointing
        // at a file whose contents were never written.
        file.sync_all()
    }
}

impl Write for Writer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Writer::Text(out) => out.write(data),
            Writer::Binary(out) => out.write(data),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Writer::Text(out) => out.flush(),
            Writer::Binary(out) => out.flush(),
        }
    }
}

/// Beside the target, so the final rename cannot cross a filesystem.
fn temporary_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    target.with_file_name(name)
}

/// Gives the temporary file the permissions of the first of `models` that
/// exists, so replacing a file cannot widen who may read it. A G-code file the
/// user kept private stays private; the set-user-ID bits are never copied.
#[cfg(unix)]
fn inherit_mode<const N: usize>(file: &File, models: [&Path; N]) {
    use std::os::unix::fs::PermissionsExt;

    let Some(mode) = models
        .iter()
        .find_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.permissions().mode() & 0o777)
    else {
        return;
    };
    let _ = file.set_permissions(fs::Permissions::from_mode(mode));
}

/// Windows permissions carry only a read-only flag, and copying that onto a
/// file still being written would block the rename that follows.
#[cfg(not(unix))]
fn inherit_mode<const N: usize>(_: &File, _: [&Path; N]) {}

/// Reads until `buffer` is full or the file ends, returning how much arrived.
fn fill(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    while read < buffer.len() {
        match file.read(&mut buffer[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this run's own, since the tests share a process.
    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("bricklayers-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a writable temp directory");
        path
    }

    fn written(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .expect("the directory exists")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_committed_sink_replaces_its_target_and_leaves_nothing_behind() {
        let directory = scratch("commit");
        let target = directory.join("part.gcode");
        fs::write(&target, "G1 X1\n").expect("seed the target");

        let source = Source::open(&target).expect("open");
        let mut sink = source.sink(&target).expect("sink");
        assert_eq!(written(&directory).len(), 2, "the temp file sits beside it");

        sink.write_all(b"G1 X2\n").expect("write");
        sink.commit().expect("commit");

        assert_eq!(fs::read_to_string(&target).expect("read back"), "G1 X2\n");
        assert_eq!(written(&directory), ["part.gcode"]);
    }

    /// A slicer hands over the only copy of its G-code, so a run that stops
    /// partway has to leave the original exactly as it was.
    #[test]
    fn a_sink_dropped_without_committing_leaves_the_target_alone() {
        let directory = scratch("abandon");
        let target = directory.join("part.gcode");
        fs::write(&target, "G1 X1\n").expect("seed the target");

        let source = Source::open(&target).expect("open");
        {
            let mut sink = source.sink(&target).expect("sink");
            sink.write_all(b"half a file").expect("write");
        }

        assert_eq!(fs::read_to_string(&target).expect("read back"), "G1 X1\n");
        assert_eq!(written(&directory), ["part.gcode"]);
    }

    /// The error names the file the caller asked for, not the temporary
    /// beside it, which only this module knows about.
    #[test]
    fn a_sink_that_cannot_be_created_names_the_target() {
        let directory = scratch("missing");
        let target = directory.join("nowhere").join("part.gcode");
        fs::write(directory.join("in.gcode"), "G1 X1\n").expect("seed the input");

        let source = Source::open(&directory.join("in.gcode")).expect("open");
        let Err(error) = source.sink(&target) else {
            panic!("the directory is missing, so the sink cannot open");
        };
        let message = error.to_string();

        assert!(message.contains("part.gcode"), "{message}");
        assert!(!message.contains(".tmp"), "{message}");
    }

    #[test]
    fn a_source_can_be_read_twice_and_written_somewhere_else() {
        let directory = scratch("rewrite");
        let input = directory.join("in.gcode");
        let output = directory.join("out.gcode");
        fs::write(&input, "; layer_height = 0.2\nG1 Z0.2\nG1 Z0.4\n").expect("seed the input");

        let source = Source::open(&input).expect("open");
        assert!(!source.is_binary());
        let survey = source.survey().expect("survey");
        assert_eq!(survey.layers, 2);

        let sink = source.sink(&output).expect("sink");
        let copied = source
            .rewrite(sink, |mut reader, writer| {
                let count = io::copy(&mut reader, writer)?;
                Ok(count)
            })
            .expect("rewrite");

        assert_eq!(
            copied as usize,
            fs::metadata(&input).expect("stat").len() as usize
        );
        assert_eq!(
            fs::read_to_string(&output).expect("read back"),
            fs::read_to_string(&input).expect("read the input")
        );
    }
}
