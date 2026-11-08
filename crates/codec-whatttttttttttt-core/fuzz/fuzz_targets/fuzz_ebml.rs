#![no_main]

//! Fuzz target for the EBML/Matroska parser: element-header
//! parse, then the full WebM/Matroska demuxer over the input.

use codec_whatttttttttttt_core::{parse_element_header, MatroskaDemuxer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_element_header(data, 0, data.len());
    let _ = MatroskaDemuxer::parse(data);
});
