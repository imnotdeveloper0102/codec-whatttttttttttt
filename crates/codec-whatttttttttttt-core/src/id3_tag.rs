//! `ID3v1` and `ID3v2` tags (`ID3v1` spec, `ID3v2`.2/2.3/2.4 spec).
//!
//! Reading covers both versions: `ID3v2` at the start of the file
//! (text frames `T*`, `COMM`, `APIC`) and `ID3v1` in the last 128
//! bytes. Writing covers `ID3v1` (always, fixed 128 bytes) and a
//! minimal `ID3v2`.3 (text frames plus `APIC` passthrough). An `APIC`
//! picture is stored as raw bytes only; the image is never decoded.
//! `ID3v1` genre names cover indices 0..=79, the original `ID3v1` list;
//! higher indices are reported as their number, not guessed.

use crate::error::{Error, Result};
use crate::frame::{Metadata, MetadataEntry};
use crate::limits::MAX_ID3_TAG_BYTES;

/// `ID3v1` tag size in bytes (fixed).
pub const ID3V1_LEN: usize = 128;

/// `ID3v1` genre names, indices 0..=79 (the original `ID3v1` list).
pub const ID3V1_GENRES: [&str; 80] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychadelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
];

/// Genre name for an `ID3v1` genre byte, or `None` for indices above
/// 79, which this library does not guess.
#[must_use]
pub fn id3v1_genre_name(index: u8) -> Option<&'static str> {
    ID3V1_GENRES.get(usize::from(index)).copied()
}

/// Genre index for a name, case-insensitive, or `None` when the name
/// is not in the 0..=79 list.
#[must_use]
pub fn id3v1_genre_index(name: &str) -> Option<u8> {
    ID3V1_GENRES
        .iter()
        .position(|genre| genre.eq_ignore_ascii_case(name))
        .and_then(|index| u8::try_from(index).ok())
}

/// One attached picture (`APIC`/`PIC` frame): raw image bytes only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedPicture {
    /// MIME type, for example `image/jpeg`.
    pub mime_type: String,
    /// Picture type byte (3 is the front cover).
    pub picture_type: u8,
    /// Description text.
    pub description: String,
    /// Raw image bytes; never decoded.
    pub data: Vec<u8>,
}

/// A parsed ID3 tag: the common fields plus every frame/entry in
/// file order, and any attached pictures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Id3Tag {
    /// Song title.
    pub title: Option<String>,
    /// Artist.
    pub artist: Option<String>,
    /// Album.
    pub album: Option<String>,
    /// Track number.
    pub track: Option<u32>,
    /// Total track count, from `TRCK` (`"3/12"`).
    pub total_tracks: Option<u32>,
    /// Year (`TYER`/`TDRC`).
    pub year: Option<String>,
    /// Genre text.
    pub genre: Option<String>,
    /// Comment text.
    pub comment: Option<String>,
    /// Attached pictures, in file order.
    pub attached_pictures: Vec<AttachedPicture>,
    /// All entries in file order, including the common fields.
    pub entries: Vec<MetadataEntry>,
}

impl Id3Tag {
    /// Sets a common field and records the entry, replacing an
    /// earlier entry with the same key.
    fn set(&mut self, key: &str, value: &[u8]) {
        self.entries.retain(|entry| entry.key != key);
        self.entries.push(MetadataEntry::new(key, value));
        if let Ok(text) = std::str::from_utf8(value) {
            let text = text.to_string();
            match key {
                "TITLE" => self.title = Some(text),
                "ARTIST" => self.artist = Some(text),
                "ALBUM" => self.album = Some(text),
                "YEAR" => self.year = Some(text),
                "GENRE" => self.genre = Some(text),
                "COMMENT" => self.comment = Some(text),
                _ => {}
            }
        }
    }

    /// Records an entry without touching the common fields.
    fn push_entry(&mut self, key: &str, value: &[u8]) {
        self.entries.push(MetadataEntry::new(key, value));
    }

    /// The tag as container metadata.
    #[must_use]
    pub fn to_metadata(&self) -> Metadata {
        Metadata {
            entries: self.entries.clone(),
        }
    }
}

/// Trims trailing NULs and spaces from a fixed-width `ID3v1` field.
fn trim_v1_field(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == 0 || bytes[end - 1] == b' ') {
        end -= 1;
    }
    &bytes[..end]
}

/// Decodes an `ID3v1` field as Latin-1 (each byte maps to the same
/// Unicode scalar), returning `None` for an empty field.
fn v1_string(bytes: &[u8]) -> Option<String> {
    let trimmed = trim_v1_field(bytes);
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.iter().map(|&b| char::from(b)).collect())
}

/// Parses the `ID3v1` tag in the last 128 bytes of `data`.
///
/// # Errors
///
/// Returns `Truncated` when fewer than 128 bytes are present and
/// `InvalidData` when the last 128 bytes do not start with `TAG`.
pub fn parse_id3v1(data: &[u8]) -> Result<Id3Tag> {
    if data.len() < ID3V1_LEN {
        return Err(Error::Truncated {
            needed: ID3V1_LEN,
            had: data.len(),
        });
    }
    let tag = &data[data.len() - ID3V1_LEN..];
    if &tag[0..3] != b"TAG" {
        return Err(Error::InvalidData {
            offset: data.len() - ID3V1_LEN,
            detail: "id3v1 tag does not start with TAG",
        });
    }
    let mut out = Id3Tag::default();
    if let Some(title) = v1_string(&tag[3..33]) {
        out.title = Some(title.clone());
        out.push_entry("TITLE", title.as_bytes());
    }
    if let Some(artist) = v1_string(&tag[33..63]) {
        out.artist = Some(artist.clone());
        out.push_entry("ARTIST", artist.as_bytes());
    }
    if let Some(album) = v1_string(&tag[63..93]) {
        out.album = Some(album.clone());
        out.push_entry("ALBUM", album.as_bytes());
    }
    if let Some(year) = v1_string(&tag[93..97]) {
        out.year = Some(year.clone());
        out.push_entry("YEAR", year.as_bytes());
    }
    // ID3v1.1: comment is 28 bytes plus a zero byte and a track
    // byte; otherwise the comment is 30 bytes and there is no
    // track number.
    if tag[125] == 0 && tag[126] != 0 {
        if let Some(comment) = v1_string(&tag[97..125]) {
            out.comment = Some(comment.clone());
            out.push_entry("COMMENT", comment.as_bytes());
        }
        let track = u32::from(tag[126]);
        out.track = Some(track);
        out.push_entry("TRACKNUMBER", track.to_string().as_bytes());
    } else if let Some(comment) = v1_string(&tag[97..127]) {
        out.comment = Some(comment.clone());
        out.push_entry("COMMENT", comment.as_bytes());
    }
    let genre_index = tag[127];
    match id3v1_genre_name(genre_index) {
        Some(name) => {
            out.genre = Some(name.to_string());
            out.push_entry("GENRE", name.as_bytes());
        }
        None => {
            out.push_entry("GENRE", genre_index.to_string().as_bytes());
        }
    }
    Ok(out)
}

/// Writes an `ID3v1` (v1.1 when a track number is present) tag: 128
/// bytes, fields truncated or NUL-padded, Latin-1 encoded.
#[must_use]
pub fn write_id3v1(tag: &Id3Tag) -> [u8; ID3V1_LEN] {
    let mut out = [0u8; ID3V1_LEN];
    out[0..3].copy_from_slice(b"TAG");
    let put = |out: &mut [u8; ID3V1_LEN], start: usize, len: usize, value: &Option<String>| {
        if let Some(text) = value {
            let bytes = text.as_bytes();
            let take = bytes.len().min(len);
            out[start..start + take].copy_from_slice(&bytes[..take]);
        }
    };
    put(&mut out, 3, 30, &tag.title);
    put(&mut out, 33, 30, &tag.artist);
    put(&mut out, 63, 30, &tag.album);
    put(&mut out, 93, 4, &tag.year);
    let comment_len = if tag.track.is_some() { 28 } else { 30 };
    put(&mut out, 97, comment_len, &tag.comment);
    if let Some(track) = tag.track {
        out[125] = 0;
        out[126] = u8::try_from(track).unwrap_or(0);
    }
    out[127] = tag
        .genre
        .as_deref()
        .and_then(id3v1_genre_index)
        .unwrap_or(255);
    out
}

/// Reads a synchsafe integer: 4 bytes, 7 bits each, big-endian.
fn read_synchsafe(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::Truncated {
        needed: offset + 4,
        had: data.len(),
    })?;
    if bytes.iter().any(|&b| b & 0x80 != 0) {
        return Err(Error::InvalidData {
            offset,
            detail: "id3 synchsafe integer has a high bit set",
        });
    }
    Ok(u32::from(bytes[0]) << 21
        | u32::from(bytes[1]) << 14
        | u32::from(bytes[2]) << 7
        | u32::from(bytes[3]))
}

/// Decodes one text frame value by its encoding byte (`ID3v2` spec
/// section 4): 0 ISO-8859-1, 1 UTF-16 with BOM, 2 UTF-16BE, 3
/// UTF-8. The value ends at the first terminator (one NUL byte, or
/// two for UTF-16).
fn decode_text_frame(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "id3 text frame has no encoding byte",
        });
    }
    let (encoding, body) = (data[0], &data[1..]);
    match encoding {
        0 => {
            // ISO-8859-1: map each byte to the same Unicode scalar.
            let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
            let text: String = body[..end].iter().map(|&b| char::from(b)).collect();
            Ok(text.into_bytes())
        }
        1 => {
            let (body, little_endian) = if body.len() >= 2 && body[0] == 0xFF && body[1] == 0xFE {
                (&body[2..], true)
            } else if body.len() >= 2 && body[0] == 0xFE && body[1] == 0xFF {
                (&body[2..], false)
            } else {
                (body, false)
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .take_while(|pair| pair[0] != 0 || pair[1] != 0)
                .map(|pair| {
                    if little_endian {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                })
                .collect();
            String::from_utf16(&units)
                .map(String::into_bytes)
                .map_err(|_| Error::InvalidData {
                    offset: 0,
                    detail: "id3 text frame is not valid utf-16",
                })
        }
        2 => {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .take_while(|pair| pair[0] != 0 || pair[1] != 0)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units)
                .map(String::into_bytes)
                .map_err(|_| Error::InvalidData {
                    offset: 0,
                    detail: "id3 text frame is not valid utf-16",
                })
        }
        3 => {
            let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
            Ok(body[..end].to_vec())
        }
        _ => Err(Error::InvalidData {
            offset: 0,
            detail: "id3 text frame encoding is not 0..=3",
        }),
    }
}

/// Reads a NUL-terminated string in `data` starting at `offset`,
/// using the frame's text encoding for the terminator width.
/// Returns the string bytes (without terminator) and the offset
/// just past the terminator.
fn read_terminated(data: &[u8], offset: usize, wide: bool) -> Result<(Vec<u8>, usize)> {
    if wide {
        let mut end = offset;
        while end + 1 < data.len() {
            if data[end] == 0 && data[end + 1] == 0 {
                return Ok((data[offset..end].to_vec(), end + 2));
            }
            end += 2;
        }
        Err(Error::Truncated {
            needed: end + 2,
            had: data.len(),
        })
    } else {
        let end = data[offset..]
            .iter()
            .position(|&b| b == 0)
            .map(|pos| offset + pos)
            .ok_or(Error::Truncated {
                needed: data.len() + 1,
                had: data.len(),
            })?;
        Ok((data[offset..end].to_vec(), end + 1))
    }
}

/// Removes unsynchronisation bytes (`0x00` after `0xFF`) from a
/// tag body, per the `ID3v2` spec.
fn remove_unsync(data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        out.push(data[i]);
        if data[i] == 0xFF && i + 1 < data.len() && data[i + 1] == 0x00 {
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// Parses one `ID3v2` frame body into the tag. `id` is the frame id
/// (`TIT2`, `APIC`, ...), `version` is 2, 3, or 4.
#[allow(clippy::too_many_lines)]
fn parse_frame(tag: &mut Id3Tag, id: &str, body: &[u8], version: u8) -> Result<()> {
    // v2.2 frame ids are 3 bytes; map them to their v2.3 names.
    let id = match id {
        "TT2" => "TIT2",
        "TP1" => "TPE1",
        "TAL" => "TALB",
        "TRK" => "TRCK",
        "TYE" => "TYER",
        "TCO" => "TCON",
        "COM" => "COMM",
        "PIC" => "APIC",
        other => other,
    };
    match id {
        "TIT2" | "TPE1" | "TALB" | "TYER" | "TDRC" | "TCON" => {
            let key = match id {
                "TIT2" => "TITLE",
                "TPE1" => "ARTIST",
                "TALB" => "ALBUM",
                "TYER" | "TDRC" => "YEAR",
                _ => "GENRE",
            };
            let text = decode_text_frame(body)?;
            if id == "TDRC" {
                // v2.4 date: keep the year part only.
                let year: Vec<u8> = text.iter().take(4).copied().collect();
                tag.set(key, &year);
            } else if id == "TCON" {
                // Genre may be "(17)" or "17" (a v1 index) or text.
                let text_str = String::from_utf8_lossy(&text).into_owned();
                let trimmed = text_str.trim_matches(|c| c == '(' || c == ')');
                if let Ok(index) = trimmed.parse::<u8>() {
                    if let Some(name) = id3v1_genre_name(index) {
                        tag.set(key, name.as_bytes());
                        return Ok(());
                    }
                }
                tag.set(key, &text);
            } else {
                tag.set(key, &text);
            }
            Ok(())
        }
        "TRCK" => {
            let text = decode_text_frame(body)?;
            let text_str = String::from_utf8_lossy(&text).into_owned();
            let mut parts = text_str.splitn(2, '/');
            if let Some(first) = parts.next() {
                if let Ok(track) = first.trim().parse::<u32>() {
                    tag.track = Some(track);
                    tag.push_entry("TRACKNUMBER", track.to_string().as_bytes());
                }
            }
            if let Some(second) = parts.next() {
                if let Ok(total) = second.trim().parse::<u32>() {
                    tag.total_tracks = Some(total);
                    tag.push_entry("TOTALTRACKS", total.to_string().as_bytes());
                }
            }
            Ok(())
        }
        "COMM" => {
            if body.len() < 4 {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "id3 COMM frame is too short",
                });
            }
            let wide = body[0] == 1 || body[0] == 2;
            // Encoding(1) + language(3), then a short description.
            let (_, after) = read_terminated(body, 4, wide)?;
            // The comment text follows the description terminator;
            // decode it with the frame encoding.
            let mut framed = vec![body[0]];
            framed.extend_from_slice(&body[after..]);
            let text = decode_text_frame(&framed)?;
            tag.set("COMMENT", &text);
            Ok(())
        }
        "APIC" | "PIC" => {
            if version == 2 {
                // v2.2 PIC: encoding(1), format(3), type(1),
                // description (terminated), data.
                if body.len() < 5 {
                    return Err(Error::InvalidData {
                        offset: 0,
                        detail: "id3 PIC frame is too short",
                    });
                }
                let format = String::from_utf8_lossy(&body[1..4]).into_owned();
                let mime_type = match format.as_str() {
                    "JPG" => "image/jpeg".to_string(),
                    "PNG" => "image/png".to_string(),
                    _ => format,
                };
                let wide = body[0] == 1 || body[0] == 2;
                let (description, after) = read_terminated(body, 5, wide)?;
                tag.attached_pictures.push(AttachedPicture {
                    mime_type,
                    picture_type: body[4],
                    description: String::from_utf8_lossy(&description).into_owned(),
                    data: body[after..].to_vec(),
                });
                tag.push_entry("APIC", &body[after..]);
                Ok(())
            } else {
                if body.is_empty() {
                    return Err(Error::InvalidData {
                        offset: 0,
                        detail: "id3 APIC frame is empty",
                    });
                }
                let wide = body[0] == 1 || body[0] == 2;
                let (mime, after_mime) = read_terminated(body, 1, false)?;
                if after_mime >= body.len() {
                    return Err(Error::Truncated {
                        needed: after_mime + 1,
                        had: body.len(),
                    });
                }
                let picture_type = body[after_mime];
                let (description, after_desc) = read_terminated(body, after_mime + 1, wide)?;
                tag.attached_pictures.push(AttachedPicture {
                    mime_type: String::from_utf8_lossy(&mime).into_owned(),
                    picture_type,
                    description: String::from_utf8_lossy(&description).into_owned(),
                    data: body[after_desc..].to_vec(),
                });
                tag.push_entry("APIC", &body[after_desc..]);
                Ok(())
            }
        }
        _ => {
            // Unknown frames are kept under their id, so no tag
            // data is silently dropped.
            tag.push_entry(id, body);
            Ok(())
        }
    }
}

/// Parses an `ID3v2` tag at the start of `data`. Returns the tag and
/// the total tag size in bytes (header + body).
///
/// # Errors
///
/// Returns `Truncated` for a cut-short header or frame,
/// `InvalidData` for a bad header, an unknown version, or a frame
/// that overruns the tag, and `AllocationLimit` when the tag
/// exceeds the ID3 tag limit.
#[allow(clippy::too_many_lines)]
pub fn parse_id3v2(data: &[u8]) -> Result<(Id3Tag, usize)> {
    if data.len() < 10 {
        return Err(Error::Truncated {
            needed: 10,
            had: data.len(),
        });
    }
    if &data[0..3] != b"ID3" {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "id3v2 tag does not start with ID3",
        });
    }
    let version = data[3];
    if !matches!(version, 2..=4) {
        return Err(Error::InvalidData {
            offset: 3,
            detail: "id3v2 version is not 2, 3, or 4",
        });
    }
    let flags = data[5];
    let unsync = flags & 0x80 != 0;
    let has_extended_header = flags & 0x40 != 0;
    let has_footer = flags & 0x10 != 0;
    let size = read_synchsafe(data, 6)? as usize;
    if size > MAX_ID3_TAG_BYTES {
        return Err(Error::AllocationLimit {
            requested: size,
            limit: MAX_ID3_TAG_BYTES,
        });
    }
    let total = 10usize
        .checked_add(size)
        .and_then(|total| total.checked_add(if has_footer { 10 } else { 0 }))
        .ok_or(Error::Overflow("id3v2 tag size overflows"))?;
    if data.len() < total {
        return Err(Error::Truncated {
            needed: total,
            had: data.len(),
        });
    }
    let body = if unsync {
        remove_unsync(&data[10..10 + size])
    } else {
        data[10..10 + size].to_vec()
    };
    let mut offset = 0usize;
    if has_extended_header {
        // Extended header: a 4-byte size (excluding the size field
        // itself in v2.3/v2.4) followed by that many bytes.
        if body.len() < 4 {
            return Err(Error::Truncated {
                needed: 4,
                had: body.len(),
            });
        }
        let ext_size = if version == 4 {
            read_synchsafe(&body, 0)? as usize
        } else {
            u32::from_be_bytes(body[0..4].try_into().unwrap_or([0; 4])) as usize
        };
        offset = 4usize
            .checked_add(ext_size)
            .ok_or(Error::Overflow("id3v2 extended header size overflows"))?;
        if offset > body.len() {
            return Err(Error::Truncated {
                needed: offset,
                had: body.len(),
            });
        }
    }
    let mut tag = Id3Tag::default();
    while offset < body.len() {
        if version == 2 {
            if body.len() - offset < 6 {
                break;
            }
            let id = &body[offset..offset + 3];
            if id.iter().all(|&b| b == 0) {
                break;
            }
            let size = (usize::from(body[offset + 3]) << 16)
                | (usize::from(body[offset + 4]) << 8)
                | usize::from(body[offset + 5]);
            let start = offset + 6;
            let end = start
                .checked_add(size)
                .ok_or(Error::Overflow("id3v2.2 frame size overflows"))?;
            if end > body.len() {
                return Err(Error::Truncated {
                    needed: end,
                    had: body.len(),
                });
            }
            let id_str = String::from_utf8_lossy(id).into_owned();
            parse_frame(&mut tag, &id_str, &body[start..end], version)?;
            offset = end;
        } else {
            if body.len() - offset < 10 {
                break;
            }
            let id = &body[offset..offset + 4];
            if id.iter().all(|&b| b == 0) {
                break;
            }
            let size = if version == 4 {
                read_synchsafe(&body, offset + 4)? as usize
            } else {
                u32::from_be_bytes(body[offset + 4..offset + 8].try_into().unwrap_or([0; 4]))
                    as usize
            };
            let start = offset + 10;
            let end = start
                .checked_add(size)
                .ok_or(Error::Overflow("id3v2 frame size overflows"))?;
            if end > body.len() {
                return Err(Error::Truncated {
                    needed: end,
                    had: body.len(),
                });
            }
            let id_str = String::from_utf8_lossy(id).into_owned();
            parse_frame(&mut tag, &id_str, &body[start..end], version)?;
            offset = end;
        }
    }
    Ok((tag, total))
}

/// Parses every ID3 tag in `data`: v2 at the start, v1 at the end.
/// v2 fields win; v1 fills the gaps.
///
/// # Errors
///
/// Returns `InvalidData` when neither a v2 tag at the start nor a
/// v1 tag at the end is present, and propagates `Truncated` or
/// `AllocationLimit` from a present-but-cut-short v2 tag.
pub fn parse_id3(data: &[u8]) -> Result<Id3Tag> {
    let mut out: Option<Id3Tag> = None;
    if data.len() >= 10 && &data[0..3] == b"ID3" {
        let (tag, _) = parse_id3v2(data)?;
        out = Some(tag);
    }
    if data.len() >= ID3V1_LEN {
        if let Ok(v1) = parse_id3v1(data) {
            match &mut out {
                Some(tag) => {
                    if tag.title.is_none() {
                        tag.title.clone_from(&v1.title);
                    }
                    if tag.artist.is_none() {
                        tag.artist.clone_from(&v1.artist);
                    }
                    if tag.album.is_none() {
                        tag.album.clone_from(&v1.album);
                    }
                    if tag.year.is_none() {
                        tag.year.clone_from(&v1.year);
                    }
                    if tag.genre.is_none() {
                        tag.genre.clone_from(&v1.genre);
                    }
                    if tag.comment.is_none() {
                        tag.comment.clone_from(&v1.comment);
                    }
                    if tag.track.is_none() {
                        tag.track = v1.track;
                    }
                    for entry in v1.entries {
                        if !tag.entries.iter().any(|e| e.key == entry.key) {
                            tag.entries.push(entry);
                        }
                    }
                }
                None => out = Some(v1),
            }
        }
    }
    out.ok_or(Error::InvalidData {
        offset: 0,
        detail: "no id3v1 or id3v2 tag found",
    })
}

/// Encodes one `ID3v2`.3 text frame (UTF-8, encoding byte 3).
fn text_frame(id: [u8; 4], text: &str) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::with_capacity(text.len() + 2);
    body.push(3);
    body.extend_from_slice(text.as_bytes());
    body.push(0);
    let mut out: Vec<u8> = Vec::with_capacity(body.len() + 10);
    out.extend_from_slice(&id);
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&body);
    out
}

/// Writes a minimal `ID3v2`.3 tag: text frames for the common fields
/// and one `APIC` frame per attached picture (bytes passthrough).
///
/// # Errors
///
/// Returns `AllocationLimit` when the tag exceeds the ID3 tag
/// limit.
pub fn write_id3v2(tag: &Id3Tag) -> Result<Vec<u8>> {
    let mut frames: Vec<u8> = Vec::new();
    if let Some(title) = &tag.title {
        frames.extend(text_frame(*b"TIT2", title));
    }
    if let Some(artist) = &tag.artist {
        frames.extend(text_frame(*b"TPE1", artist));
    }
    if let Some(album) = &tag.album {
        frames.extend(text_frame(*b"TALB", album));
    }
    if let Some(track) = tag.track {
        let text = match tag.total_tracks {
            Some(total) => format!("{track}/{total}"),
            None => track.to_string(),
        };
        frames.extend(text_frame(*b"TRCK", &text));
    }
    if let Some(year) = &tag.year {
        frames.extend(text_frame(*b"TYER", year));
    }
    if let Some(genre) = &tag.genre {
        frames.extend(text_frame(*b"TCON", genre));
    }
    if let Some(comment) = &tag.comment {
        // COMM: encoding(1), language(3), description (empty +
        // terminator), text + terminator.
        let mut body: Vec<u8> = Vec::new();
        body.push(3);
        body.extend_from_slice(b"eng");
        body.push(0);
        body.extend_from_slice(comment.as_bytes());
        body.push(0);
        frames.extend_from_slice(b"COMM");
        frames.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(&body);
    }
    for picture in &tag.attached_pictures {
        let mut body: Vec<u8> = Vec::new();
        body.push(3);
        body.extend_from_slice(picture.mime_type.as_bytes());
        body.push(0);
        body.push(picture.picture_type);
        body.extend_from_slice(picture.description.as_bytes());
        body.push(0);
        body.extend_from_slice(&picture.data);
        frames.extend_from_slice(b"APIC");
        frames.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(&body);
    }
    if frames.len() > MAX_ID3_TAG_BYTES {
        return Err(Error::AllocationLimit {
            requested: frames.len(),
            limit: MAX_ID3_TAG_BYTES,
        });
    }
    let size =
        u32::try_from(frames.len()).map_err(|_| Error::Overflow("id3v2 tag size exceeds u32"))?;
    let mut out: Vec<u8> = Vec::with_capacity(frames.len() + 10);
    out.extend_from_slice(b"ID3");
    out.push(3);
    out.push(0);
    out.push(0);
    out.push(u8::try_from((size >> 21) & 0x7F).unwrap_or(0));
    out.push(u8::try_from((size >> 14) & 0x7F).unwrap_or(0));
    out.push(u8::try_from((size >> 7) & 0x7F).unwrap_or(0));
    out.push(u8::try_from(size & 0x7F).unwrap_or(0));
    out.extend_from_slice(&frames);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v1_fixture() -> Vec<u8> {
        let mut data = vec![0u8; 200];
        let tag = write_id3v1(&Id3Tag {
            title: Some("Title".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            year: Some("2026".to_string()),
            genre: Some("Rock".to_string()),
            comment: Some("Note".to_string()),
            track: Some(3),
            ..Id3Tag::default()
        });
        data.extend_from_slice(&tag);
        data
    }

    #[test]
    fn v1_round_trip() {
        let tag = parse_id3v1(&v1_fixture()).unwrap();
        assert_eq!(tag.title.as_deref(), Some("Title"));
        assert_eq!(tag.artist.as_deref(), Some("Artist"));
        assert_eq!(tag.album.as_deref(), Some("Album"));
        assert_eq!(tag.year.as_deref(), Some("2026"));
        assert_eq!(tag.genre.as_deref(), Some("Rock"));
        assert_eq!(tag.comment.as_deref(), Some("Note"));
        assert_eq!(tag.track, Some(3));
    }

    #[test]
    fn v1_without_tag_is_invalid_data() {
        let err = parse_id3v1(&[0u8; 200]).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn v1_short_input_is_truncated() {
        let err = parse_id3v1(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn v1_unknown_genre_index_is_numeric() {
        let mut data = vec![0u8; 200];
        let mut tag = write_id3v1(&Id3Tag::default());
        tag[127] = 200;
        data.extend_from_slice(&tag);
        let parsed = parse_id3v1(&data).unwrap();
        assert_eq!(parsed.genre.as_deref(), None);
        let entry = parsed
            .entries
            .iter()
            .find(|entry| entry.key == "GENRE")
            .unwrap();
        assert_eq!(entry.as_str(), Some("200"));
    }

    #[test]
    fn v2_round_trip() {
        let tag = Id3Tag {
            title: Some("T".to_string()),
            artist: Some("A".to_string()),
            album: Some("Al".to_string()),
            track: Some(3),
            total_tracks: Some(12),
            year: Some("2026".to_string()),
            genre: Some("Rock".to_string()),
            comment: Some("C".to_string()),
            attached_pictures: vec![AttachedPicture {
                mime_type: "image/jpeg".to_string(),
                picture_type: 3,
                description: "cover".to_string(),
                data: vec![0xFF, 0xD8, 0xFF, 0xE0],
            }],
            ..Id3Tag::default()
        };
        let bytes = write_id3v2(&tag).unwrap();
        let (parsed, total) = parse_id3v2(&bytes).unwrap();
        assert_eq!(total, bytes.len());
        assert_eq!(parsed.title.as_deref(), Some("T"));
        assert_eq!(parsed.artist.as_deref(), Some("A"));
        assert_eq!(parsed.album.as_deref(), Some("Al"));
        assert_eq!(parsed.track, Some(3));
        assert_eq!(parsed.total_tracks, Some(12));
        assert_eq!(parsed.year.as_deref(), Some("2026"));
        assert_eq!(parsed.genre.as_deref(), Some("Rock"));
        assert_eq!(parsed.comment.as_deref(), Some("C"));
        assert_eq!(parsed.attached_pictures.len(), 1);
        assert_eq!(parsed.attached_pictures[0].mime_type, "image/jpeg");
        assert_eq!(parsed.attached_pictures[0].picture_type, 3);
        assert_eq!(
            parsed.attached_pictures[0].data,
            vec![0xFF, 0xD8, 0xFF, 0xE0]
        );
    }

    #[test]
    fn v2_genre_number_resolves() {
        let mut tag = Id3Tag {
            genre: Some("(17)".to_string()),
            ..Id3Tag::default()
        };
        // Write with the numeric form and parse it back.
        let bytes = write_id3v2(&tag).unwrap();
        let (parsed, _) = parse_id3v2(&bytes).unwrap();
        assert_eq!(parsed.genre.as_deref(), Some("Rock"));
        tag.genre = Some("17".to_string());
        let bytes = write_id3v2(&tag).unwrap();
        let (parsed, _) = parse_id3v2(&bytes).unwrap();
        assert_eq!(parsed.genre.as_deref(), Some("Rock"));
    }

    #[test]
    fn v2_truncated_tag_is_truncated() {
        let bytes = write_id3v2(&Id3Tag {
            title: Some("T".to_string()),
            ..Id3Tag::default()
        })
        .unwrap();
        let cut = &bytes[..bytes.len() - 2];
        let err = parse_id3v2(cut).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }

    #[test]
    fn v2_bad_version_is_invalid_data() {
        let mut bytes = write_id3v2(&Id3Tag::default()).unwrap();
        bytes[3] = 9;
        let err = parse_id3v2(&bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }

    #[test]
    fn parse_id3_prefers_v2_over_v1() {
        let mut data = write_id3v2(&Id3Tag {
            title: Some("V2".to_string()),
            ..Id3Tag::default()
        })
        .unwrap();
        data.extend(vec![0u8; 100]);
        data.extend_from_slice(&write_id3v1(&Id3Tag {
            title: Some("V1".to_string()),
            artist: Some("V1Artist".to_string()),
            ..Id3Tag::default()
        }));
        let tag = parse_id3(&data).unwrap();
        assert_eq!(tag.title.as_deref(), Some("V2"));
        assert_eq!(tag.artist.as_deref(), Some("V1Artist"));
    }

    #[test]
    fn parse_id3_with_no_tag_is_invalid_data() {
        let err = parse_id3(b"hello world, no tags here at all....").unwrap_err();
        assert!(matches!(err, Error::InvalidData { .. }));
    }
}
