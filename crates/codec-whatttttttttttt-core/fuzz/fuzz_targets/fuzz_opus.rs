#![no_main]

//! Fuzz target for the Opus packet parser: TOC/packet parse,
//! OpusHead parse, and OpusTags parse over the same input.

use codec_whatttttttttttt_core::{
    parse_opus_head, parse_opus_packet, parse_opus_tags,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_opus_packet(data);
    let _ = parse_opus_head(data);
    let _ = parse_opus_tags(data);
});
