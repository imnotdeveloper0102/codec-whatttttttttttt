#![no_main]

//! Fuzz target for the Vorbis setup parser: the three headers
//! (identification, comment, setup) are parsed from the input
//! split into three roughly equal parts, matching the order a
//! real Vorbis stream presents them in.

use codec_whatttttttttttt_core::vorbis_setup::{
    parse_vorbis_comment, parse_vorbis_identification,
};
use codec_whatttttttttttt_core::VorbisSetup;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_vorbis_identification(data);
    let _ = parse_vorbis_comment(data);
    let third = data.len() / 3;
    let _ = VorbisSetup::from_headers(
        &data[..third],
        &data[third..third.saturating_mul(2)],
        &data[third.saturating_mul(2)..],
    );
});
