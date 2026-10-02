//! Error and result types shared across the crate.

use std::path::PathBuf;

/// Convenience alias for `Result<T, afni_io::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while reading or writing AFNI/SUMA files.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An I/O error, annotated with the path that triggered it.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },

    /// An I/O error without an associated path.
    #[error(transparent)]
    RawIo(#[from] std::io::Error),

    /// The byte stream was not valid UTF-8 where text was required.
    #[error("invalid UTF-8 in {context}: {source}")]
    Utf8 {
        /// What was being decoded.
        context: String,
        /// The underlying error.
        source: std::str::Utf8Error,
    },

    /// The data did not match the format being parsed.
    #[error("parse error: {0}")]
    Parse(String),

    /// A required attribute, field, or element was missing.
    #[error("missing {0}")]
    Missing(String),

    /// A numeric or structural value was out of the legal range.
    #[error("invalid value: {0}")]
    Invalid(String),

    /// The format or sub-format is recognised but not yet implemented.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl Error {
    /// Build a [`Error::Parse`] from anything string-like.
    pub fn parse(msg: impl Into<String>) -> Self {
        Error::Parse(msg.into())
    }

    /// Build a [`Error::Missing`] from anything string-like.
    pub fn missing(msg: impl Into<String>) -> Self {
        Error::Missing(msg.into())
    }

    /// Build a [`Error::Invalid`] from anything string-like.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    /// Build a [`Error::Unsupported`] from anything string-like.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Error::Unsupported(msg.into())
    }
}

/// Internal helper: read a file, attaching the path to any I/O error.
pub(crate) fn read_file(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Internal helper: write a file, attaching the path to any I/O error.
pub(crate) fn write_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Internal helper: decode bytes as UTF-8 with a descriptive context.
pub(crate) fn from_utf8(bytes: &[u8], context: &str) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|source| Error::Utf8 {
            context: context.to_owned(),
            source,
        })
}
