#![no_main]

//! Fuzz target for the FLAC parser: STREAMINFO parse, whole
//! stream parse, then one frame decode through the frame decoder.

use codec_whatttttttttttt_core::{
    decode_flac_frame, parse_flac_stream, parse_streaminfo,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 34 {
        let _ = parse_streaminfo(&data[..34]);
    }
    let Ok(stream) = parse_flac_stream(data) else {
        return;
    };
    let offset = stream.first_frame_offset.min(data.len());
    let _ = decode_flac_frame(&data[offset..], &stream.streaminfo);
});
