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
        "{path} has already been bricked; running again would stack a second shift on \
         the first. Re-slice, or pass --force if that is what you want"
    )]
    AlreadyProcessed { path: PathBuf },
}

impl Error {
    pub(crate) fn io(path: impl AsRef<Path>, source: io::Error) -> Self {
        Error::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}
