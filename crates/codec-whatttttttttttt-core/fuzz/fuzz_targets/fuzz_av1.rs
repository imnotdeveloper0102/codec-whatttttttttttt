#![no_main]

//! Fuzz target for the AV1 OBU parser: OBU header and full OBU
//! parse from the start of the input.

use codec_whatttttttttttt_core::{parse_obu, parse_obu_header};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_obu_header(data, 0);
    let _ = parse_obu(data, 0);
});
