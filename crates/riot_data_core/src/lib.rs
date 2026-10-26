//! What every `riot_data` crate shares: the [`DataError`] that names the
//! file and the place in it, and [`math`], trigonometry that gives the same
//! bits on every machine.

mod error;
pub mod math;

pub use error::{DataError, Result, line_of};
