#![no_main]

//! Fuzz target for the WAV parser: header parse, then PCM decode
//! of the declared data chunk through the decoder.

use codec_whatttttttttttt_core::{parse_wav_header, Codec, Decoder, Packet};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(header) = parse_wav_header(data) else {
        return;
    };
    let Ok(mut decoder) = Decoder::from_wav_header(&header) else {
        return;
    };
    let start = header.data_offset.min(data.len());
    let end = start
        .saturating_add(header.data_size as usize)
        .min(data.len());
    let packet = Packet {
        codec: Codec::Pcm,
        data: data[start..end].to_vec(),
        timestamp: 0,
        is_keyframe: true,
    };
    let _ = decoder.decode(&packet);
});
