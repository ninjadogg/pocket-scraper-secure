use std::fmt;

/// Errors are plain messages meant for the person using the extension. They never
/// include secret material or record contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Input failed validation (recipe, message, extractor output).
    Invalid(String),
    /// Decryption or authentication failed: wrong passphrase or tampered data.
    Crypto(&'static str),
}

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(message) => f.write_str(message),
            Error::Crypto(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}
