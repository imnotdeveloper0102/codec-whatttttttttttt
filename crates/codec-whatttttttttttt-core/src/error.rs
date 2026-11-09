//! Typed errors for the whole library.
//!
//! Every failure of a public API is one of these variants. Library code
//! never panics on untrusted input and never uses `unwrap` or `expect`
//! outside tests; a corrupt file is `InvalidData` or `Truncated`, never a
//! partial frame presented as valid.

use thiserror::Error;

/// The single error type returned by all public APIs in this crate.
#[derive(Debug, Error)]
pub enum Error {
    /// An underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Data that fails validation, with the byte offset where it failed.
    /// A CRC mismatch is reported with this variant.
    #[error("invalid data at offset {offset}: {detail}")]
    InvalidData { offset: usize, detail: &'static str },

    /// Input ended before a complete value could be read.
    #[error("truncated input: needed {needed} bytes, had {had}")]
    Truncated { needed: usize, had: usize },

    /// A well-formed feature that this version does not implement.
    #[error("unsupported feature: {0}")]
    Unsupported(&'static str),

    /// A numeric value outside the range this library accepts.
    #[error("value out of range: {0}")]
    Overflow(&'static str),

    /// A requested allocation exceeds the limit in `limits.rs`.
    #[error("allocation limit exceeded: requested {requested} bytes, limit {limit} bytes")]
    AllocationLimit { requested: usize, limit: usize },

    /// A caller-supplied argument is invalid (null-equivalent, zero where
    /// a positive value is required, or inconsistent with other arguments).
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    /// A licensed codec was requested without a valid license. The
    /// codec is named so the caller can tell which license is
    /// missing; no software fallback exists for these codecs.
    #[error("license required for codec {codec}")]
    LicenseRequired { codec: &'static str },
}

/// Convenience alias used by every public function in this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_displays_needed_and_had() {
        let err = Error::Truncated {
            needed: 44,
            had: 20,
        };
        assert!(err.to_string().contains("44"));
        assert!(err.to_string().contains("20"));
    }

    #[test]
    fn invalid_data_carries_offset() {
        let err = Error::InvalidData {
            offset: 8,
            detail: "bad magic",
        };
        assert!(matches!(err, Error::InvalidData { offset: 8, .. }));
    }
}
