#![no_main]

//! Fuzz target for the ADTS parser: header parse, whole-frame
//! verification (including CRC-16 when present), then a full
//! drain through the streaming demuxer.

use codec_whatttttttttttt_core::{parse_adts_header, verify_adts_frame, AdtsDemuxer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_adts_header(data);
    let _ = verify_adts_frame(data);
    let Ok(mut demuxer) = AdtsDemuxer::from_bytes(data) else {
        return;
    };
    while let Ok(Some(_frame)) = demuxer.next_frame() {}
});
