//! MP4 `meta`/`ilst`/`udta` tag parsing (ISO/IEC 14496-12).
//!
//! Tags live in an `ilst` box inside a `meta` box, which sits at
//! the top level or inside `udta` (in `moov` or at the top level).
//! Each `ilst` entry is keyed by a 4-byte type (`©nam`, `©ART`,
//! `©alb`, `trkn`, ...) and holds one or more `data` boxes: a
//! type indicator (1 = UTF-8 text, 21 = big-endian integer), a
//! locale, and the payload. Well-known keys are mapped to the
//! canonical metadata names (`TITLE`, `ARTIST`, `ALBUM`, ...);
//! `gnre` (genre index) resolves through the `ID3v1` genre table;
//! `trkn` yields `TRACKNUMBER` and `TOTALTRACKS`; `covr` bytes
//! are kept raw, never decoded as an image.

use crate::error::{Error, Result};
use crate::frame::{Metadata, MetadataEntry};
use crate::id3_tag::id3v1_genre_name;
use crate::mp4_box::{BoxCursor, BoxHeader};

/// Box types read by this module.
const BOX_META: [u8; 4] = *b"meta";
const BOX_UDTA: [u8; 4] = *b"udta";
const BOX_MOOV: [u8; 4] = *b"moov";
const BOX_ILST: [u8; 4] = *b"ilst";
const BOX_DATA: [u8; 4] = *b"data";

/// `data` box type indicator for UTF-8 text (ISO/IEC 14496-12).
const DATA_TYPE_UTF8: u32 = 1;

/// `data` box type indicator for a big-endian integer.
const DATA_TYPE_INTEGER: u32 = 21;

/// Metadata name for an `ilst` key: the canonical name for a
/// known key, the raw 4 bytes lossily decoded for an unknown one.
fn key_name(key: [u8; 4]) -> String {
    match canonical_key(key) {
        Some(name) => name.to_string(),
        None => String::from_utf8_lossy(&key).into_owned(),
    }
}

/// Canonical metadata name for a known `ilst` key, or `None`.
fn canonical_key(key: [u8; 4]) -> Option<&'static str> {
    match &key {
        b"\xA9nam" => Some("TITLE"),
        b"\xA9ART" => Some("ARTIST"),
        b"aART" => Some("ALBUM_ARTIST"),
        b"\xA9alb" => Some("ALBUM"),
        b"\xA9day" => Some("DATE"),
        b"\xA9gen" | b"gnre" => Some("GENRE"),
        b"trkn" => Some("TRACKNUMBER"),
        b"disk" => Some("DISCNUMBER"),
        b"\xA9too" => Some("ENCODER"),
        b"desc" => Some("DESCRIPTION"),
        b"\xA9cmt" => Some("COMMENT"),
        b"cprt" => Some("COPYRIGHT"),
        b"\xA9grp" => Some("GROUPING"),
        b"\xA9wrt" => Some("COMPOSER"),
        b"covr" => Some("COVER"),
        _ => None,
    }
}

/// Track/disk number: 8 bytes, reserved(2), number(2),
/// total(2), reserved(2) — all big-endian u16. A zero number or
/// total means "unknown" and is skipped.
fn parse_track_or_disk(
    payload: &[u8],
    header: &BoxHeader,
    key: [u8; 4],
    name: &str,
    metadata: &mut Metadata,
) -> Result<()> {
    if payload.len() < 6 {
        return Err(Error::InvalidData {
            offset: header.offset,
            detail: "mp4 trkn/disk data is too short",
        });
    }
    let number = u16::from_be_bytes([payload[2], payload[3]]);
    let total = u16::from_be_bytes([payload[4], payload[5]]);
    if number != 0 {
        metadata
            .entries
            .push(MetadataEntry::new(name, number.to_string().as_bytes()));
    }
    if total != 0 {
        let total_key = if key == *b"trkn" {
            "TOTALTRACKS"
        } else {
            "TOTALDISCS"
        };
        metadata
            .entries
            .push(MetadataEntry::new(total_key, total.to_string().as_bytes()));
    }
    Ok(())
}

/// Genre index: a big-endian integer (usually 2 bytes, 1-based
/// per the iTunes convention, 0 = none). iTunes stores the
/// `ID3v1` index + 1; an index with no known v1 name is kept as
/// its decimal string.
fn parse_genre(payload: &[u8], header: &BoxHeader, metadata: &mut Metadata) -> Result<()> {
    if payload.is_empty() {
        return Err(Error::InvalidData {
            offset: header.offset,
            detail: "mp4 gnre data is empty",
        });
    }
    let mut index: u32 = 0;
    for &byte in payload {
        index = index
            .checked_mul(256)
            .and_then(|value| value.checked_add(u32::from(byte)))
            .ok_or(Error::Overflow("mp4 gnre index overflows"))?;
    }
    if index == 0 {
        return Ok(());
    }
    let v1_index = u8::try_from(index - 1).map_err(|_| Error::InvalidData {
        offset: header.offset,
        detail: "mp4 gnre index exceeds u8",
    })?;
    match id3v1_genre_name(v1_index) {
        Some(genre) => {
            metadata
                .entries
                .push(MetadataEntry::new("GENRE", genre.as_bytes()));
        }
        None => {
            metadata
                .entries
                .push(MetadataEntry::new("GENRE", index.to_string().as_bytes()));
        }
    }
    Ok(())
}

/// Parses one `data` box payload into metadata entries for `key`.
/// `data_start`/`data_end` bound the box payload (after the
/// 8-byte header); the first 8 payload bytes are the type
/// indicator and the locale.
fn parse_data_box(
    data: &[u8],
    header: &BoxHeader,
    key: [u8; 4],
    metadata: &mut Metadata,
) -> Result<()> {
    if header.payload_len() < 8 {
        return Err(Error::InvalidData {
            offset: header.offset,
            detail: "mp4 tag data box is too short",
        });
    }
    let type_indicator = u32::from_be_bytes(
        data[header.data_start..header.data_start + 4]
            .try_into()
            .unwrap_or([0; 4]),
    );
    let payload = &data[header.data_start + 8..header.data_end];
    let name = key_name(key);
    match &key {
        b"trkn" | b"disk" => parse_track_or_disk(payload, header, key, &name, metadata),
        b"gnre" => parse_genre(payload, header, metadata),
        b"covr" => {
            // Cover art bytes: kept raw, never decoded.
            metadata.entries.push(MetadataEntry::new("COVER", payload));
            Ok(())
        }
        _ => {
            if type_indicator == DATA_TYPE_UTF8 || type_indicator == 0 {
                metadata.entries.push(MetadataEntry::new(&name, payload));
                Ok(())
            } else if type_indicator == DATA_TYPE_INTEGER {
                let mut value: u64 = 0;
                for &byte in payload {
                    value = value
                        .checked_mul(256)
                        .and_then(|v| v.checked_add(u64::from(byte)))
                        .ok_or(Error::Overflow("mp4 tag integer overflows"))?;
                }
                metadata
                    .entries
                    .push(MetadataEntry::new(&name, value.to_string().as_bytes()));
                Ok(())
            } else {
                // Other data types: keep the raw bytes.
                metadata.entries.push(MetadataEntry::new(&name, payload));
                Ok(())
            }
        }
    }
}

/// Parses an `ilst` box into metadata. `ilst_start`/`ilst_end`
/// bound the box payload (after the 8-byte header).
fn parse_ilst(data: &[u8], ilst_start: usize, ilst_end: usize) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    let mut cursor = BoxCursor::new(data, ilst_start, ilst_end)?;
    while let Some(entry) = cursor.next_header()? {
        let key = entry.box_type;
        let mut children = BoxCursor::new(data, entry.data_start, entry.data_end)?;
        let mut saw_data = false;
        while let Some(child) = children.next_header()? {
            if child.box_type == BOX_DATA {
                saw_data = true;
                parse_data_box(data, &child, key, &mut metadata)?;
            }
        }
        if !saw_data {
            return Err(Error::InvalidData {
                offset: entry.offset,
                detail: "mp4 ilst entry has no data box",
            });
        }
    }
    Ok(metadata)
}

/// Parses a `meta` box (a `FullBox`: 4 version/flags bytes, then
/// children) into metadata from its `ilst` child, if any.
fn parse_meta_box(data: &[u8], meta: &BoxHeader) -> Result<Metadata> {
    if meta.payload_len() < 4 {
        return Err(Error::Truncated {
            needed: meta.data_start + 4,
            had: data.len(),
        });
    }
    let mut cursor = BoxCursor::new(data, meta.data_start + 4, meta.data_end)?;
    while let Some(child) = cursor.next_header()? {
        if child.box_type == BOX_ILST {
            return parse_ilst(data, child.data_start, child.data_end);
        }
    }
    Ok(Metadata::new())
}

/// Parses tags from a whole MP4 file: top-level `meta` and
/// `udta`/`meta`, plus `moov`/`udta`/`meta`. Returns empty
/// metadata when the file has no tags.
///
/// # Errors
///
/// Returns `Truncated` for a cut-short box and `InvalidData` for
/// a malformed `ilst` entry; a file with no tag boxes returns
/// empty metadata, not an error.
pub fn parse_mp4_tags(data: &[u8]) -> Result<Metadata> {
    parse_mp4_metadata(data)
}

/// Parses tags from a whole MP4 file (see `parse_mp4_tags`).
///
/// # Errors
///
/// The same errors as `parse_mp4_tags`.
pub fn parse_mp4_metadata(data: &[u8]) -> Result<Metadata> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            needed: 8,
            had: data.len(),
        });
    }
    let mut metadata = Metadata::new();
    let mut cursor = BoxCursor::new(data, 0, data.len())?;
    while let Some(top) = cursor.next_header()? {
        match top.box_type {
            BOX_META => {
                metadata.entries.extend(parse_meta_box(data, &top)?.entries);
            }
            BOX_UDTA => {
                let mut inner = BoxCursor::new(data, top.data_start, top.data_end)?;
                while let Some(child) = inner.next_header()? {
                    if child.box_type == BOX_META {
                        metadata
                            .entries
                            .extend(parse_meta_box(data, &child)?.entries);
                    }
                }
            }
            BOX_MOOV => {
                let mut inner = BoxCursor::new(data, top.data_start, top.data_end)?;
                while let Some(child) = inner.next_header()? {
                    if child.box_type == BOX_UDTA {
                        let mut udta = BoxCursor::new(data, child.data_start, child.data_end)?;
                        while let Some(field) = udta.next_header()? {
                            if field.box_type == BOX_META {
                                metadata
                                    .entries
                                    .extend(parse_meta_box(data, &field)?.entries);
                            }
                        }
                    }
                    if child.box_type == BOX_META {
                        metadata
                            .entries
                            .extend(parse_meta_box(data, &child)?.entries);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4_box::{parse_box_header, BoxHeader};

    /// Builds a box: 4-byte size, 4-byte type, payload.
    fn boxed(box_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u32::try_from(payload.len() + 8).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(&box_type);
        out.extend_from_slice(payload);
        out
    }

    /// Builds a `data` box: type indicator, locale 0, payload.
    fn data_box(type_indicator: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&type_indicator.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(payload);
        boxed(*b"data", &body)
    }

    fn ilst_file() -> Vec<u8> {
        let mut ilst_body = Vec::new();
        let mut title = Vec::new();
        title.extend_from_slice(&data_box(DATA_TYPE_UTF8, b"Song Title"));
        ilst_body.extend_from_slice(&boxed(*b"\xA9nam", &title));
        let mut track = Vec::new();
        let trkn_payload = vec![0u8, 0, 0, 3, 0, 12, 0, 0];
        track.extend_from_slice(&data_box(DATA_TYPE_INTEGER, &trkn_payload));
        ilst_body.extend_from_slice(&boxed(*b"trkn", &track));
        let mut meta_body = vec![0u8; 4];
        meta_body.extend_from_slice(&boxed(*b"ilst", &ilst_body));
        let meta = boxed(*b"meta", &meta_body);
        let mut moov_body = Vec::new();
        moov_body.extend_from_slice(&boxed(*b"udta", &meta));
        // Minimal file: ftyp + moov(udta(meta(ilst))). parse only
        // walks boxes, so no mvhd/trak is needed here.
        let mut file = boxed(*b"ftyp", b"isom");
        file.extend_from_slice(&boxed(*b"moov", &moov_body));
        file
    }

    #[test]
    fn parses_title_and_track() {
        let metadata = parse_mp4_tags(&ilst_file()).unwrap();
        assert_eq!(metadata.get("TITLE").unwrap().as_str(), Some("Song Title"));
        assert_eq!(metadata.get("TRACKNUMBER").unwrap().as_str(), Some("3"));
        assert_eq!(metadata.get("TOTALTRACKS").unwrap().as_str(), Some("12"));
    }

    #[test]
    fn parses_top_level_meta() {
        let mut meta_body = vec![0u8; 4];
        let mut ilst_body = Vec::new();
        ilst_body.extend_from_slice(&boxed(
            *b"\xA9alb",
            &data_box(DATA_TYPE_UTF8, b"Album Name"),
        ));
        meta_body.extend_from_slice(&boxed(*b"ilst", &ilst_body));
        let mut file = boxed(*b"ftyp", b"isom");
        file.extend_from_slice(&boxed(*b"meta", &meta_body));
        let metadata = parse_mp4_tags(&file).unwrap();
        assert_eq!(metadata.get("ALBUM").unwrap().as_str(), Some("Album Name"));
    }

    #[test]
    fn no_tags_returns_empty_metadata() {
        let file = boxed(*b"ftyp", b"isom");
        let metadata = parse_mp4_tags(&file).unwrap();
        assert_eq!(metadata.entries.len(), 0);
    }

    #[test]
    fn gnre_resolves_genre_name() {
        let mut ilst_body = Vec::new();
        // gnre value 18 (1-based) = ID3v1 index 17 = Rock.
        ilst_body.extend_from_slice(&boxed(*b"gnre", &data_box(DATA_TYPE_INTEGER, &[0, 18])));
        let mut meta_body = vec![0u8; 4];
        meta_body.extend_from_slice(&boxed(*b"ilst", &ilst_body));
        let mut file = boxed(*b"ftyp", b"isom");
        file.extend_from_slice(&boxed(*b"meta", &meta_body));
        let metadata = parse_mp4_tags(&file).unwrap();
        assert_eq!(metadata.get("GENRE").unwrap().as_str(), Some("Rock"));
    }

    #[test]
    fn cover_bytes_are_kept_raw() {
        let mut ilst_body = Vec::new();
        ilst_body.extend_from_slice(&boxed(*b"covr", &data_box(13, &[0xFF, 0xD8, 0xFF, 0xE0])));
        let mut meta_body = vec![0u8; 4];
        meta_body.extend_from_slice(&boxed(*b"ilst", &ilst_body));
        let mut file = boxed(*b"ftyp", b"isom");
        file.extend_from_slice(&boxed(*b"meta", &meta_body));
        let metadata = parse_mp4_tags(&file).unwrap();
        let cover = metadata.get("COVER").unwrap();
        assert_eq!(cover.value, vec![0xFF, 0xD8, 0xFF, 0xE0]);
        assert!(!cover.is_valid_utf8);
    }

    #[test]
    fn short_input_is_truncated() {
        let err = parse_mp4_tags(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn box_header_is_reusable() {
        let data = boxed(*b"ftyp", b"isom");
        let header: BoxHeader = parse_box_header(&data, 0, data.len()).unwrap();
        assert_eq!(header.box_type, *b"ftyp");
    }
}
