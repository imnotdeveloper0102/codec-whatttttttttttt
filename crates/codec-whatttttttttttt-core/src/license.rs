//! License gate for patent-encumbered codecs (H.264/H.265/AAC).
//!
//! This module only checks the shape of a caller-supplied license
//! and whether it has expired. It performs no online verification,
//! makes no network call, holds no key, and has no bypass: a codec
//! behind the gate stays closed until `validate` succeeds for it.
//! The check is deliberately local because the library cannot
//! establish the legal standing of a license on its own; the
//! caller is responsible for obtaining a real license from the
//! relevant patent pool, and this module only records that the
//! presented license was well-formed and unexpired at call time.

use crate::error::{Error, Result};
use std::time::{SystemTime, UNIX_EPOCH};

/// Shortest license id accepted, in characters.
pub const MIN_LICENSE_ID_LEN: usize = 8;
/// Longest license id accepted, in characters.
pub const MAX_LICENSE_ID_LEN: usize = 64;

/// Kind of license presented. The kind is recorded for audit
/// purposes only; it does not change what the gate accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LicenseType {
    /// A paid commercial license.
    Commercial,
    /// A time-limited evaluation license.
    Evaluation,
    /// An internal license, covering internal use and testing.
    Internal,
}

/// A license presented by the caller to open a licensed codec
/// through the hardware path in `hw`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseContext {
    /// Who the license was issued to.
    pub licensee: String,
    /// Kind of license.
    pub license_type: LicenseType,
    /// License identifier, opaque to this library.
    pub license_id: String,
    /// Expiry as seconds since the Unix epoch.
    pub expires: u64,
    /// What the license covers (for example a codec name or a
    /// product name); recorded for audit purposes only.
    pub scope: String,
    /// In-memory audit trail of every `validate` call.
    audit_log: Vec<AuditRecord>,
}

/// One entry in the audit trail: when a license was checked, for
/// which codec, and whether the gate opened. The license id is
/// never stored raw, only masked, so a log dump cannot leak it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// Seconds since the Unix epoch when the check ran.
    pub timestamp_secs: u64,
    /// Codec the check was for (`"avc"`, `"hevc"`, `"aac"`).
    pub codec: &'static str,
    /// License id with only its first four characters visible.
    pub license_id_masked: String,
    /// True when the gate opened.
    pub granted: bool,
    /// Why the check passed or failed.
    pub reason: &'static str,
}

/// Rules for a well-formed license id: 8..=64 characters, ASCII
/// letters, digits, or `-` only. The bounds keep a truncated or
/// pasted fragment from passing, and the alphabet keeps the id
/// safe to log masked and to compare without normalization.
fn license_id_is_well_formed(id: &str) -> bool {
    let len = id.chars().count();
    (MIN_LICENSE_ID_LEN..=MAX_LICENSE_ID_LEN).contains(&len)
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn now_secs() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Error::InvalidArgument("system time is before the unix epoch"))
}

impl LicenseContext {
    /// Builds a context, rejecting a malformed license up front.
    /// Expiry is not checked here: an expired license must still
    /// be constructible so `validate` can report it as
    /// `LicenseRequired` with an audit entry, instead of the
    /// caller never learning why the gate stayed closed.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the licensee or scope is
    /// empty or the license id breaks the format rules in
    /// `license_id_is_well_formed`.
    pub fn new(
        licensee: String,
        license_type: LicenseType,
        license_id: String,
        expires: u64,
        scope: String,
    ) -> Result<Self> {
        if licensee.is_empty() {
            return Err(Error::InvalidArgument("licensee must not be empty"));
        }
        if scope.is_empty() {
            return Err(Error::InvalidArgument("license scope must not be empty"));
        }
        if !license_id_is_well_formed(&license_id) {
            return Err(Error::InvalidArgument(
                "license id must be 8..=64 ASCII letters, digits, or '-'",
            ));
        }
        Ok(Self {
            licensee,
            license_type,
            license_id,
            expires,
            scope,
            audit_log: Vec::new(),
        })
    }

    /// The license id with only its first four characters visible,
    /// the rest replaced by `***`. Ids shorter than four
    /// characters (rejected by `new`, but possible via a struct
    /// literal) mask to `***` alone.
    #[must_use]
    pub fn license_id_masked(&self) -> String {
        let visible: String = self.license_id.chars().take(4).collect();
        if visible.chars().count() < 4 {
            return "***".to_string();
        }
        format!("{visible}***")
    }

    /// Checks this license for one codec and records the outcome
    /// in the audit log. Format is re-checked here because a
    /// context built with a struct literal bypasses `new`.
    ///
    /// An expired license is `LicenseRequired`, not
    /// `InvalidArgument`: the license itself is well-formed, it
    /// simply no longer grants access, and the caller needs to
    /// tell "renew the license" apart from "fix the license
    /// fields". A malformed license stays `InvalidArgument`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a malformed license or an
    /// unreadable system clock, and `LicenseRequired` when the
    /// license has expired.
    pub fn validate(&mut self, codec: &'static str) -> Result<()> {
        let now = now_secs()?;
        let masked = self.license_id_masked();
        let record = |granted: bool, reason: &'static str| AuditRecord {
            timestamp_secs: now,
            codec,
            license_id_masked: masked.clone(),
            granted,
            reason,
        };
        if self.licensee.is_empty() || self.scope.is_empty() {
            self.audit_log
                .push(record(false, "empty licensee or scope"));
            return Err(Error::InvalidArgument(
                "licensee and scope must not be empty",
            ));
        }
        if !license_id_is_well_formed(&self.license_id) {
            self.audit_log.push(record(false, "malformed license id"));
            return Err(Error::InvalidArgument(
                "license id must be 8..=64 ASCII letters, digits, or '-'",
            ));
        }
        if self.expires <= now {
            self.audit_log.push(record(false, "license expired"));
            return Err(Error::LicenseRequired { codec });
        }
        self.audit_log.push(record(true, "license valid"));
        Ok(())
    }

    /// The audit trail, oldest first. Every `validate` call appends
    /// exactly one entry, granted or denied.
    #[must_use]
    pub fn audit_log(&self) -> &[AuditRecord] {
        &self.audit_log
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_fixture() -> LicenseContext {
        LicenseContext::new(
            "test licensee".to_string(),
            LicenseType::Commercial,
            "TEST-12345678".to_string(),
            4_102_444_800,
            "avc".to_string(),
        )
        .unwrap()
    }

    #[test]
    fn rejects_license_id_with_disallowed_characters() {
        let err = LicenseContext::new(
            "licensee".to_string(),
            LicenseType::Evaluation,
            "bad id!".to_string(),
            4_102_444_800,
            "avc".to_string(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn rejects_short_license_id() {
        let err = LicenseContext::new(
            "licensee".to_string(),
            LicenseType::Internal,
            "ABC-1".to_string(),
            4_102_444_800,
            "avc".to_string(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)));
    }

    #[test]
    fn expired_license_is_license_required_and_audited() {
        let mut ctx = LicenseContext::new(
            "licensee".to_string(),
            LicenseType::Commercial,
            "TEST-12345678".to_string(),
            1,
            "avc".to_string(),
        )
        .unwrap();
        let err = ctx.validate("avc").unwrap_err();
        assert!(matches!(err, Error::LicenseRequired { codec: "avc" }));
        assert_eq!(ctx.audit_log().len(), 1);
        assert!(!ctx.audit_log()[0].granted);
        assert_eq!(ctx.audit_log()[0].license_id_masked, "TEST***");
        assert!(!ctx.audit_log()[0].license_id_masked.contains("12345678"));
    }

    #[test]
    fn valid_license_grants_and_masks_id_in_audit_log() {
        let mut ctx = valid_fixture();
        ctx.validate("hevc").unwrap();
        assert_eq!(ctx.audit_log().len(), 1);
        assert!(ctx.audit_log()[0].granted);
        assert_eq!(ctx.audit_log()[0].license_id_masked, "TEST***");
    }
}
