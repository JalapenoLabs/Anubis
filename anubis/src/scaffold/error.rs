//! The error every scaffold argument rejection carries.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

/// A scaffold the generator refuses to plan, with the reason.
///
/// Scaffolding is a one-shot command, so the caller's only sane response to a
/// rejected argument is to print the reason and stop. One error type carries
/// that reason for the whole planning stage: a malformed field, an
/// unsupported field type, an ownership chain the generator cannot express.
/// The message is written for a terminal and names the fix wherever one
/// exists.
pub struct ScaffoldError {
    message: String,
    backtrace: Backtrace,
}

impl ScaffoldError {
    /// Builds an error carrying `message`, capturing a backtrace.
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
        }
    }

    /// The reason the scaffold was refused.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for ScaffoldError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScaffoldError")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl Display for ScaffoldError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for ScaffoldError {}

impl From<super::NameError> for ScaffoldError {
    fn from(error: super::NameError) -> Self {
        Self::new(error.message().to_owned())
    }
}
