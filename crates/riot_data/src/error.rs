//! One error type for everything `riot_data` reads: which file, where in it,
//! and what is wrong.

use std::fmt;
use std::path::{Path, PathBuf};

type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A data file could not be read, parsed or resolved.
///
/// Displays as `path: context: message`, for example
/// `data/maps/city.json: layer "solids", object 12: unknown solid type "buildng"`.
#[derive(Debug)]
pub struct DataError {
    /// The file the error is about, if there is one.
    pub path: Option<PathBuf>,
    /// Where in the file, outermost first: `layer "solids", object 12`.
    pub context: Vec<String>,
    pub message: String,
    source: Option<Source>,
}

impl DataError {
    pub fn new(message: impl Into<String>) -> Self {
        DataError {
            path: None,
            context: Vec::new(),
            message: message.into(),
            source: None,
        }
    }

    /// An error caused by another one (I/O, JSON, RON).
    pub fn caused_by(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        DataError {
            source: Some(Box::new(source)),
            ..DataError::new(message)
        }
    }

    /// Adds an outer context (the innermost is added first).
    pub fn at(mut self, context: impl Into<String>) -> Self {
        self.context.insert(0, context.into());
        self
    }

    /// Names the file, unless an inner error already named one (an error in
    /// a tileset or tunables file referenced by another file keeps its own).
    pub fn in_file(mut self, path: &Path) -> Self {
        if self.path.is_none() {
            self.path = Some(path.to_path_buf());
        }
        self
    }

    /// Reading `path` failed.
    pub fn io(path: &Path, e: std::io::Error) -> Self {
        DataError::caused_by(format!("cannot read: {e}"), e).in_file(path)
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(p) = &self.path {
            write!(f, "{}: ", p.display())?;
        }
        for c in &self.context {
            write!(f, "{c}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for DataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// `Result` with a [`DataError`].
pub type Result<T, E = DataError> = std::result::Result<T, E>;

/// Turns a RON error into a [`DataError`] with its line and column.
pub(crate) fn ron_error(e: ron::error::SpannedError) -> DataError {
    let p = e.span.start;
    DataError::caused_by(format!("line {}, column {}: {}", p.line, p.col, e.code), e)
}

/// Line (1-based) of the first occurrence of `needle` in `text`, for errors
/// found after parsing.
pub(crate) fn line_of(text: &str, needle: &str) -> Option<usize> {
    text.find(needle)
        .map(|at| text[..at].bytes().filter(|&b| b == b'\n').count() + 1)
}
