//! Vorbis comment packets (Vorbis I specification section 5).
//!
//! A Vorbis comment is the second header of a Vorbis stream, and
//! the same packet format is the metadata block of FLAC and the
//! comment header of Opus. This module completes what
//! `vorbis_setup::parse_vorbis_comment` starts: it parses the full
//! packet (magic, vendor string, comments, framing bit) strictly,
//! writes it back, and exposes the standard keys with
//! case-insensitive lookup (keys are ASCII case-insensitive per
//! section 5.2). The lenient header parser in `vorbis_setup.rs`
//! is left alone; this module is the strict packet-level API used
//! by metadata writers and by callers that need round-tripping.
//!
//! Keys are `0x20..=0x7E` ASCII excluding `=` (section 5.2). A
//! comment without `=` is kept under an empty key, matching the
//! FLAC metadata parser in `flac_frame.rs`.

use crate::error::{Error, Result};
use crate::frame::{Metadata, MetadataEntry};
use crate::limits::MAX_METADATA_BYTES;

/// Packet type byte of a Vorbis comment header (section 4.2.1).
pub const VORBIS_COMMENT_PACKET_TYPE: u8 = 3;

/// Standard Vorbis comment keys (Vorbis I specification section
/// 5, "Content vector format" examples).
pub const STANDARD_KEYS: &[&str] = &[
    "TITLE",
    "VERSION",
    "ALBUM",
    "TRACKNUMBER",
    "ARTIST",
    "PERFORMER",
    "COPYRIGHT",
    "LICENSE",
    "ORGANIZATION",
    "DESCRIPTION",
    "GENRE",
    "DATE",
    "LOCATION",
    "CONTACT",
    "ISRC",
];

/// A parsed Vorbis comment packet: vendor string plus entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VorbisComment {
    /// Vendor string, raw bytes (usually UTF-8).
    pub vendor: Vec<u8>,
    /// Comment entries in packet order.
    pub entries: Vec<MetadataEntry>,
}

impl VorbisComment {
    /// Creates an empty comment with the given vendor string.
    #[must_use]
    pub fn new(vendor: &[u8]) -> Self {
        Self {
            vendor: vendor.to_vec(),
            entries: Vec::new(),
        }
    }

    /// First entry whose key matches `key` case-insensitively
    /// (ASCII), or `None`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&MetadataEntry> {
        self.entries
            .iter()
            .find(|entry| entry.key.eq_ignore_ascii_case(key))
    }

    /// Value of the first entry for `key` as text, or `None` when
    /// absent or not valid UTF-8.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(MetadataEntry::as_str)
    }

    /// Sets `key` to `value`, replacing the first entry with a
    /// case-insensitively equal key and dropping any later
    /// duplicates, or appending when absent. The key is stored
    /// uppercased.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the key is empty, contains
    /// `=`, or has a byte outside `0x20..=0x7E`.
    pub fn set(&mut self, key: &str, value: &[u8]) -> Result<()> {
        validate_key(key)?;
        let key = key.to_ascii_uppercase();
        self.entries
            .retain(|entry| !entry.key.eq_ignore_ascii_case(&key));
        self.entries.push(MetadataEntry::new(&key, value));
        Ok(())
    }

    /// Removes every entry whose key matches `key`
    /// case-insensitively. Returns `true` when anything was
    /// removed.
    #[must_use]
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| !entry.key.eq_ignore_ascii_case(key));
        self.entries.len() != before
    }

    /// The comment as container metadata: the vendor under
    /// `VENDOR`, then the entries in packet order.
    #[must_use]
    pub fn to_metadata(&self) -> Metadata {
        let mut metadata = Metadata::new();
        metadata
            .entries
            .push(MetadataEntry::new("VENDOR", &self.vendor));
        metadata.entries.extend(self.entries.iter().cloned());
        metadata
    }

    /// Builds a comment from container metadata: the `VENDOR`
    /// entry becomes the vendor string, the rest become entries.
    #[must_use]
    pub fn from_metadata(metadata: &Metadata) -> Self {
        let mut comment = Self::default();
        for entry in &metadata.entries {
            if entry.key == "VENDOR" {
                comment.vendor.clone_from(&entry.value);
            } else {
                comment.entries.push(entry.clone());
            }
        }
        comment
    }

    /// Parses a full Vorbis comment packet: `0x03 "vorbis"`,
    /// vendor length + vendor, comment count + comments, framing
    /// bit 1 (section 5). Unlike the lenient header parser in
    /// `vorbis_setup.rs`, a missing framing bit or a length that
    /// overruns the packet is an error here.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the packet ends early,
    /// `InvalidData` for a bad magic or an unset framing bit, and
    /// `AllocationLimit` when the packet exceeds the metadata
    /// limit.
    pub fn parse(data: &[u8]) -> Result<Self> {
        parse_vorbis_comment_packet(data)
    }

    /// Serializes the comment: `0x03 "vorbis"`, vendor, comments,
    /// framing bit 1.
    ///
    /// # Errors
    ///
    /// Returns `AllocationLimit` when the serialized packet
    /// exceeds the metadata limit.
    pub fn write(&self) -> Result<Vec<u8>> {
        write_vorbis_comment_packet(self)
    }
}

/// Validates a Vorbis comment key (section 5.2): non-empty, ASCII
/// `0x20..=0x7E`, no `=`.
///
/// # Errors
///
/// Returns `InvalidArgument` when the key breaks any of those
/// rules.
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Error::InvalidArgument(
            "vorbis comment key must not be empty",
        ));
    }
    if !key.bytes().all(|b| (0x20..=0x7E).contains(&b) && b != b'=') {
        return Err(Error::InvalidArgument(
            "vorbis comment key must be ASCII 0x20..=0x7E without '='",
        ));
    }
    Ok(())
}

/// Reads a little-endian `u32` at `offset`.
///
/// # Errors
///
/// Returns `Truncated` when the value runs past the input.
fn read_u32_le(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: data.len(),
    })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Parses a full Vorbis comment packet (section 5).
///
/// # Errors
///
/// Returns `Truncated` when the packet ends early, `InvalidData`
/// for a bad magic or an unset framing bit, and `AllocationLimit`
/// when the packet exceeds the metadata limit.
pub fn parse_vorbis_comment_packet(data: &[u8]) -> Result<VorbisComment> {
    if data.len() > MAX_METADATA_BYTES {
        return Err(Error::AllocationLimit {
            requested: data.len(),
            limit: MAX_METADATA_BYTES,
        });
    }
    if data.len() < 7 {
        return Err(Error::Truncated {
            needed: 7,
            had: data.len(),
        });
    }
    if data[0] != VORBIS_COMMENT_PACKET_TYPE || &data[1..7] != b"vorbis" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "vorbis comment magic mismatch",
        });
    }
    let mut offset = 7usize;
    let vendor_len = read_u32_le(data, offset)? as usize;
    offset += 4;
    let vendor = data
        .get(offset..offset + vendor_len)
        .ok_or(Error::Truncated {
            needed: offset + vendor_len,
            had: data.len(),
        })?
        .to_vec();
    offset += vendor_len;
    let count = read_u32_le(data, offset)?;
    offset += 4;
    // Every comment needs at least its 4-byte length, which bounds
    // the loop by the input size.
    if usize::try_from(count).unwrap_or(usize::MAX) > data.len() / 4 {
        return Err(Error::InvalidData {
            offset,
            detail: "vorbis comment count exceeds packet size",
        });
    }
    let mut entries: Vec<MetadataEntry> = Vec::new();
    for _ in 0..count {
        let len = read_u32_le(data, offset)? as usize;
        offset += 4;
        let raw = data.get(offset..offset + len).ok_or(Error::Truncated {
            needed: offset + len,
            had: data.len(),
        })?;
        offset += len;
        match raw.iter().position(|&b| b == b'=') {
            Some(equals) => {
                let key = String::from_utf8_lossy(&raw[..equals]).into_owned();
                entries.push(MetadataEntry::new(&key, &raw[equals + 1..]));
            }
            None => {
                entries.push(MetadataEntry::new("", raw));
            }
        }
    }
    if offset >= data.len() {
        return Err(Error::Truncated {
            needed: offset + 1,
            had: data.len(),
        });
    }
    if data[offset] & 0x01 == 0 {
        return Err(Error::InvalidData {
            offset,
            detail: "vorbis comment framing bit must be 1",
        });
    }
    Ok(VorbisComment { vendor, entries })
}

/// Serializes a Vorbis comment packet (section 5).
///
/// # Errors
///
/// Returns `AllocationLimit` when the serialized packet exceeds
/// the metadata limit.
pub fn write_vorbis_comment_packet(comment: &VorbisComment) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    out.push(VORBIS_COMMENT_PACKET_TYPE);
    out.extend_from_slice(b"vorbis");
    out.extend_from_slice(
        &u32::try_from(comment.vendor.len())
            .map_err(|_| Error::Overflow("vorbis vendor string exceeds u32"))?
            .to_le_bytes(),
    );
    out.extend_from_slice(&comment.vendor);
    out.extend_from_slice(
        &u32::try_from(comment.entries.len())
            .map_err(|_| Error::Overflow("vorbis comment count exceeds u32"))?
            .to_le_bytes(),
    );
    for entry in &comment.entries {
        let raw_len = entry
            .key
            .len()
            .checked_add(1)
            .and_then(|len| len.checked_add(entry.value.len()))
            .ok_or(Error::Overflow(
                "vorbis comment entry exceeds address space",
            ))?;
        if out.len() + 4 + raw_len + 1 > MAX_METADATA_BYTES {
            return Err(Error::AllocationLimit {
                requested: out.len() + 4 + raw_len + 1,
                limit: MAX_METADATA_BYTES,
            });
        }
        out.extend_from_slice(
            &u32::try_from(raw_len)
                .map_err(|_| Error::Overflow("vorbis comment entry exceeds u32"))?
                .to_le_bytes(),
        );
        out.extend_from_slice(entry.key.as_bytes());
        out.push(b'=');
        out.extend_from_slice(&entry.value);
    }
    out.push(1);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> VorbisComment {
        let mut comment = VorbisComment::new(b"test vendor");
        comment.set("TITLE", b"Song").unwrap();
        comment.set("ARTIST", b"Singer").unwrap();
        comment.set("TRACKNUMBER", b"3").unwrap();
        comment
    }

    #[test]
    fn round_trip() {
        let comment = fixture();
        let bytes = comment.write().unwrap();
        let parsed = VorbisComment::parse(&bytes).unwrap();
        assert_eq!(parsed.vendor, b"test vendor");
        assert_eq!(parsed.get_str("title"), Some("Song"));
        assert_eq!(parsed.get_str("ARTIST"), Some("Singer"));
        assert_eq!(parsed.get_str("tracknumber"), Some("3"));
        assert_eq!(parsed.entries.len(), 3);
    }

    #[test]
    fn set_replaces_case_insensitively() {
        let mut comment = fixture();
        comment.set("title", b"Other").unwrap();
        assert_eq!(comment.entries.len(), 3);
        assert_eq!(comment.get_str("TITLE"), Some("Other"));
        assert!(comment.remove("ArTiSt"));
        assert_eq!(comment.get("ARTIST"), None);
        assert_eq!(comment.entries.len(), 2);
    }

    #[test]
    fn invalid_keys_are_rejected() {
        let mut comment = VorbisComment::default();
        assert!(matches!(
            comment.set("", b"x"),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            comment.set("A=B", b"x"),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            comment.set("T\u{00C9}", b"x"),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn bad_magic_is_invalid_data() {
        let mut bytes = fixture().write().unwrap();
        bytes[1] = b'X';
        let err = VorbisComment::parse(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn unset_framing_bit_is_invalid_data() {
        let mut bytes = fixture().write().unwrap();
        let last = bytes.len() - 1;
        bytes[last] = 0;
        let err = VorbisComment::parse(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn truncated_comment_is_truncated() {
        let bytes = fixture().write().unwrap();
        let cut = &bytes[..bytes.len() - 3];
        let err = VorbisComment::parse(cut).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn comment_without_equals_keeps_empty_key() {
        let mut comment = VorbisComment::new(b"v");
        comment.entries.push(MetadataEntry::new("", b"bare"));
        let bytes = comment.write().unwrap();
        let parsed = VorbisComment::parse(&bytes).unwrap();
        assert_eq!(parsed.entries[0].key, "");
        assert_eq!(parsed.entries[0].as_str(), Some("bare"));
    }

    #[test]
    fn metadata_round_trip() {
        let comment = fixture();
        let metadata = comment.to_metadata();
        assert_eq!(
            metadata.get("VENDOR").unwrap().as_str(),
            Some("test vendor")
        );
        let back = VorbisComment::from_metadata(&metadata);
        assert_eq!(back, comment);
    }
}
