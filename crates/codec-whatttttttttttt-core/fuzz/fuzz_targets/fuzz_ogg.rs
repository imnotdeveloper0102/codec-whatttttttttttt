#![no_main]

//! Fuzz target for the Ogg page parser: walk every page in the
//! input, parsing each header and its segment table.

use codec_whatttttttttttt_core::parse_ogg_page;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut offset = 0;
    while offset < data.len() {
        match parse_ogg_page(&data[offset..]) {
            Ok((_page, consumed)) => {
                if consumed == 0 {
                    break;
                }
                offset += consumed;
            }
            Err(_) => break,
        }
    }
});
