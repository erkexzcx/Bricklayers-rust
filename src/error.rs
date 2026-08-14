use std::io;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{path} is not valid binary G-code: {reason}")]
    Bgcode { path: PathBuf, reason: String },

    #[error(
        "{path} does not read as G-code: {saw}. corbel rewrites the file it is \
         given, so it will not touch one it cannot recognise; point it at a \
         sliced .gcode or .bgcode file, or pass --force if that is what you want"
    )]
    NotGcode { path: PathBuf, saw: String },

    #[error(
        "{path} has already been {done}; running again would stack a second pass on \
         the first. Re-slice, or pass --force if that is what you want"
    )]
    AlreadyProcessed { path: PathBuf, done: &'static str },
}

impl Error {
    pub(crate) fn io(path: impl AsRef<Path>, source: io::Error) -> Self {
        Error::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}
