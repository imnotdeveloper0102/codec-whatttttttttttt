#![no_main]

//! Fuzz target for the MP4 box parser: walk every box in the
//! input, then the full MP4 demuxer over the input.
//!
//! The `avc1`/`hev1`/`mp4a` sample entries in `stsd` are parsed
//! by `Mp4Demuxer::parse` (via the private `parse_stsd`), so
//! they are only reached when the input contains a valid `moov`
//! — random mutations almost never build one. The seed corpus
//! in `fuzz/corpus/fuzz_mp4/` therefore includes the real
//! passthrough fixtures (`avc_aac.mp4`, `hevc_aac.mp4`,
//! `sine_aac.m4a`) so mutations start from valid `stsd`
//! structures. The common `Demuxer` is driven as well, since
//! it lists the same tracks through a second code path.

use codec_whatttttttttttt_core::{parse_box_header, Demuxer, Mp4Demuxer};
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
    if let Ok(mut demuxer) = Demuxer::from_bytes(data) {
        while let Ok(Some(_packet)) = demuxer.next_packet() {}
    }
});
