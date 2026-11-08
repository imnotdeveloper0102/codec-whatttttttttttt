//! Integration tests for EBML parsing (Matroska specification).
//!
//! The vectors are hand-built byte strings, so they do not depend
//! on an external encoder: every width, bias, and bound is checked
//! against the spec text, not against another implementation.

use codec_whatttttttttttt_core::{
    read_element_id, read_float, read_signed_vint, read_sint, read_string, read_uint, read_vint,
    read_vint_unknown, EbmlCursor, Error,
};

#[test]
fn vint_widths_and_values() {
    // Width 1: marker 1xxxxxxx, value is the low 7 bits.
    assert_eq!(read_vint(&[0x80], 0).unwrap(), (0, 1));
    assert_eq!(read_vint(&[0xFF], 0).unwrap(), (127, 1));
    // Width 2: marker 01xxxxxx.
    assert_eq!(read_vint(&[0x40, 0x00], 0).unwrap(), (0, 2));
    assert_eq!(read_vint(&[0x7F, 0xFF], 0).unwrap(), (16_383, 2));
    // Width 3.
    assert_eq!(read_vint(&[0x20, 0x00, 0x00], 0).unwrap(), (0, 3));
    // Width 4.
    assert_eq!(read_vint(&[0x10, 0x00, 0x00, 0x00], 0).unwrap(), (0, 4));
    // Width 8.
    assert_eq!(
        read_vint(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01], 0).unwrap(),
        (1, 8)
    );
    // Offset is honored.
    assert_eq!(read_vint(&[0x00, 0xBF], 1).unwrap(), (63, 1));
}

#[test]
fn vint_invalid_and_truncated() {
    // No marker bit: not a vint.
    let err = read_vint(&[0x00], 0).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
    // Marker says width 2, only one byte present.
    let err = read_vint(&[0x40], 0).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
    // Offset past the end.
    let err = read_vint(&[0x80], 1).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn vint_unknown_size_is_all_ones() {
    // Width 1 unknown: 0xFF.
    assert_eq!(read_vint_unknown(&[0xFF], 0).unwrap(), (None, 1));
    // Width 2 unknown: 0x7FFF.
    assert_eq!(read_vint_unknown(&[0x7F, 0xFF], 0).unwrap(), (None, 2));
    // Width 2 known: 0x7FFE is 16382, not unknown.
    assert_eq!(
        read_vint_unknown(&[0x7F, 0xFE], 0).unwrap(),
        (Some(16_382), 2)
    );
    // A plain value is not unknown.
    assert_eq!(read_vint_unknown(&[0x8A], 0).unwrap(), (Some(10), 1));
}

#[test]
fn signed_vint_bias_per_width() {
    // Width 1 bias 63: 0x80 is unsigned 0 -> -63, 0xBF is 63 -> 0.
    assert_eq!(read_signed_vint(&[0x80], 0).unwrap(), (-63, 1));
    assert_eq!(read_signed_vint(&[0xBF], 0).unwrap(), (0, 1));
    assert_eq!(read_signed_vint(&[0xC0], 0).unwrap(), (1, 1));
    // Width 2 bias 8191: 0x40 0x00 is unsigned 0 -> -8191.
    assert_eq!(read_signed_vint(&[0x40, 0x00], 0).unwrap(), (-8191, 2));
}

#[test]
fn element_id_widths() {
    assert_eq!(
        read_element_id(&[0x1A, 0x45, 0xDF, 0xA3], 0).unwrap(),
        (0x1A45_DFA3, 4)
    );
    assert_eq!(read_element_id(&[0x42, 0x82], 0).unwrap(), (0x4282, 2));
    assert_eq!(read_element_id(&[0xAE], 0).unwrap(), (0xAE, 1));
    assert_eq!(read_element_id(&[0x53, 0xAB], 0).unwrap(), (0x53AB, 2));
    let err = read_element_id(&[0x00], 0).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn element_size_over_parent_is_invalid_data() {
    // EBML header element declaring 100 bytes inside a 10-byte
    // parent: the size overruns the parent, so it is corrupt data,
    // not a truncated file.
    let data = [0x1A, 0x45, 0xDF, 0xA3, 0xE4, 0, 0, 0, 0, 0];
    let mut cursor = EbmlCursor::new(&data, 0, data.len()).unwrap();
    let err = cursor.next_header().unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn element_header_truncated_is_truncated() {
    // One byte only: not even a full element id.
    let data = [0x1A];
    let mut cursor = EbmlCursor::new(&data, 0, data.len()).unwrap();
    let err = cursor.next_header().unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
    // Id present, size missing.
    let data = [0x42, 0x82];
    let mut cursor = EbmlCursor::new(&data, 0, data.len()).unwrap();
    let err = cursor.next_header().unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn cursor_iterates_children_and_skips_unknown() {
    // A master element (id 0xAE, size 8) holding two children:
    // 0xD7 (track number, 1 byte) and an unknown 0x42 0x99
    // (2 bytes), each skipped or read by size.
    let data = [
        0xAE, 0x88, // TrackEntry, size 8
        0xD7, 0x81, 0x01, // TrackNumber = 1
        0x42, 0x99, 0x82, 0xAA, 0xBB, // unknown, size 2
    ];
    let mut cursor = EbmlCursor::new(&data, 0, data.len()).unwrap();
    let master = cursor.next_header().unwrap().unwrap();
    assert_eq!(master.id, 0xAE);
    assert_eq!(master.payload_len(), 8);
    let mut inner = EbmlCursor::new(&data, master.data_start, master.data_end).unwrap();
    let first = inner.next_header().unwrap().unwrap();
    assert_eq!(first.id, 0xD7);
    assert_eq!(
        read_uint(&data, first.data_start, first.payload_len()).unwrap(),
        1
    );
    let second = inner.next_header().unwrap().unwrap();
    assert_eq!(second.id, 0x4299);
    assert_eq!(second.payload_len(), 2);
    assert!(inner.next_header().unwrap().is_none());
}

#[test]
fn read_uint_sint_float_string() {
    assert_eq!(read_uint(&[0x01, 0x02], 0, 2).unwrap(), 0x0102);
    assert_eq!(read_uint(&[], 0, 0).unwrap(), 0);
    assert_eq!(read_sint(&[0xFF], 0, 1).unwrap(), -1);
    assert_eq!(read_sint(&[0x80, 0x00], 0, 2).unwrap(), -32_768);
    assert_eq!(read_sint(&[0x7F, 0xFF], 0, 2).unwrap(), 32_767);
    assert_eq!(read_float(&[0x3F, 0x80, 0x00, 0x00], 0, 4).unwrap(), 1.0);
    assert_eq!(
        read_float(&[0x3F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 0, 8).unwrap(),
        1.0
    );
    assert_eq!(read_string(b"webm\0", 0, 5).unwrap(), "webm");
    let err = read_string(&[0xFF, 0xFE], 0, 2).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
    let err = read_uint(&[0x01], 0, 2).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
    let err = read_sint(&[], 0, 0).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}
