#![no_main]

//! Fuzz target for the MP3 frame header parser and Layer III
//! frame decoder: parse the header at the start of the input,
//! then decode the whole frame through `Mp3Decoder` when the
//! header parses.

use codec_whatttttttttttt_core::{Mp3Decoder, Mp3FrameHeader};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(header) = Mp3FrameHeader::parse(data) else {
        return;
    };
    // Only a complete frame reaches the decoder; a header
    // whose declared length runs past the input is a
    // truncation, not a decode.
    if header.frame_length == 0 || data.len() < header.frame_length {
        return;
    }
    let mut decoder = Mp3Decoder::new();
    let _ = decoder.decode_frame(&data[..header.frame_length]);
});
