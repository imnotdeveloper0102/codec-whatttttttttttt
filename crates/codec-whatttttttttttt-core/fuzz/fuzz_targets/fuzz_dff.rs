#![no_main]

//! Fuzz target for the DFF parser: probe the format, parse the
//! header, drain the chunk packets through the demuxer, and
//! decimate the first packets to PCM through the decoder, so
//! mutated chunk layouts reach the decimator too.

use std::io::Cursor;

use codec_whatttttttttttt_core::{probe_dsd, Decoder, DsdDemuxer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = probe_dsd(data);
    let Ok(mut demuxer) = DsdDemuxer::new(Cursor::new(data)) else {
        return;
    };
    let Ok(mut decoder) = Decoder::from_dsd_header(demuxer.header()) else {
        return;
    };
    while let Ok(Some(packet)) = demuxer.next_packet() {
        let _ = decoder.decode(&packet);
    }
    let _ = decoder.flush();
});
