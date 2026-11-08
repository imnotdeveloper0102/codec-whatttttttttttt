#![no_main]

//! Fuzz target for the VP9 bitstream parser: superframe index,
//! frame-header parse, then the full frame parse, each from a
//! fresh reference state.

use codec_whatttttttttttt_core::{
    parse_superframe, parse_vp9_frame, parse_vp9_frame_header, Vp9ReferenceState,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_superframe(data);
    let _ = parse_vp9_frame_header(data, &mut Vp9ReferenceState::default());
    let _ = parse_vp9_frame(data, &mut Vp9ReferenceState::default());
});
