#![no_main]

//! Fuzz target for the MP4 box parser: walk every box in the
//! input, then the full MP4 demuxer over the input.

use codec_whatttttttttttt_core::{parse_box_header, Mp4Demuxer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut offset = 0;
    while offset < data.len() {
        match parse_box_header(data, offset, data.len()) {
            Ok(header) => {
                if header.data_end <= offset {
                    break;
                }
                offset = header.data_end;
            }
            Err(_) => break,
        }
    }
    let _ = Mp4Demuxer::parse(data);
});
