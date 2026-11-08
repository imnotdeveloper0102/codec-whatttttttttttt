//! Property tests for the trust boundary: bitreader, PCM
//! conversion, FLAC round-trip, resample identity, EBML varints,
//! and Ogg pages.

use codec_whatttttttttttt_core::{read_vint, resample, AudioFrame, BitReader, OggPage, PcmFormat};
use proptest::prelude::*;

/// Encodes `value` as an EBML variable-length integer of exactly
/// `width` bytes (RFC 8794 section 4.1): the marker bit sits at
/// `0x80 >> (width - 1)` and the remaining bits hold the value.
fn encode_vint(value: u64, width: u32) -> Vec<u8> {
    let mut out = vec![0u8; width as usize];
    let marker = 0x80u8 >> (width - 1);
    // Mask before narrowing: the top bits fit in the marker's
    // free bits by construction (value < 2^(7 * width)).
    let top = (value >> (8 * (width - 1))) & u64::from(marker - 1);
    out[0] = marker | u8::try_from(top).unwrap();
    for i in 1..width {
        let byte = (value >> (8 * (width - 1 - i))) & 0xFF;
        out[i as usize] = u8::try_from(byte).unwrap();
    }
    out
}

/// Packs `value` as `width` MSB-first bits into bytes.
fn pack_bits(value: u32, width: u32) -> Vec<u8> {
    let byte_count = width.div_ceil(8) as usize;
    let mut out = vec![0u8; byte_count];
    for i in 0..width {
        let bit = (value >> (width - 1 - i)) & 1;
        if bit == 1 {
            let byte_index = (i / 8) as usize;
            let bit_index = 7 - (i % 8);
            out[byte_index] |= 1 << bit_index;
        }
    }
    out
}

proptest! {
    #[test]
    fn bitreader_round_trip_preserves_value(value in any::<u32>(), width in 1u32..=32) {
        let masked = if width == 32 { value } else { value & ((1u32 << width) - 1) };
        let bytes = pack_bits(masked, width);
        let mut reader = BitReader::new(&bytes);
        prop_assert_eq!(reader.read_bits(width).unwrap(), masked);
    }

    #[test]
    fn flac_encode_decode_round_trip(
        samples in prop::collection::vec(-32768i16..=32767i16, 1..8192),
        channels in 1u32..=2,
        sample_rate in prop::sample::select(vec![8000u32, 44100, 48000]),
    ) {
        // Interleaved samples must be a whole number of frames.
        let frames = samples.len() / channels as usize;
        prop_assume!(frames > 0);
        let samples = &samples[..frames * channels as usize];
        let flac = codec_whatttttttttttt_core::encode_flac_stream(
            samples,
            channels,
            sample_rate,
        )
        .unwrap();
        let stream =
            codec_whatttttttttttt_core::parse_flac_stream(&flac).unwrap();
        let mut decoded: Vec<f32> = Vec::new();
        let mut offset = stream.first_frame_offset;
        while offset < flac.len() {
            let (frame, consumed) =
                codec_whatttttttttttt_core::decode_flac_frame(
                    &flac[offset..],
                    &stream.streaminfo,
                )
                .unwrap();
            prop_assert!(consumed > 0);
            decoded.extend_from_slice(&frame.samples);
            offset += consumed;
        }
        prop_assert_eq!(decoded.len(), samples.len());
        for (original, back) in samples.iter().zip(decoded.iter()) {
            let expected = f32::from(*original) / 32768.0;
            prop_assert!((expected - back).abs() < 1.0 / 32768.0 + f32::EPSILON);
        }
    }

    #[test]
    fn resample_identity_at_same_rate(
        samples in prop::collection::vec(-1.0f32..=1.0f32, 0..512),
        channels in 1u32..=2,
    ) {
        let frame = AudioFrame {
            sample_rate: 44100,
            channels,
            samples: samples.clone(),
            timestamp: 0,
        };
        let out = resample(&frame, 44100).unwrap();
        prop_assert_eq!(out.sample_rate, 44100);
        prop_assert_eq!(out.channels, channels);
        prop_assert_eq!(out.samples, samples);
    }

    #[test]
    fn ebml_vint_round_trip(
        width in 1u32..=8,
        raw in any::<u64>(),
    ) {
        let value = raw % (1u64 << (7 * width));
        let bytes = encode_vint(value, width);
        let (back, consumed) = read_vint(&bytes, 0).unwrap();
        prop_assert_eq!(back, value);
        prop_assert_eq!(consumed, width as usize);
    }

    #[test]
    fn ogg_page_round_trip(
        segments in prop::collection::vec(any::<u8>(), 0..8),
        granule_position in any::<i64>(),
        serial in any::<u32>(),
        sequence in any::<u32>(),
        header_type in 0u8..=7,
    ) {
        let data_len: usize =
            segments.iter().map(|&size| usize::from(size)).sum();
        let mut data = vec![0u8; data_len];
        // Fill the payload with a deterministic pattern so a
        // writer that drops bytes is caught.
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = u8::try_from(i % 251).unwrap();
        }
        let page = OggPage {
            header_type,
            granule_position,
            serial,
            sequence,
            segments: segments.clone(),
            data: data.clone(),
        };
        let bytes = codec_whatttttttttttt_core::write_ogg_page(&page).unwrap();
        let (back, consumed) =
            codec_whatttttttttttt_core::parse_ogg_page(&bytes).unwrap();
        prop_assert_eq!(consumed, bytes.len());
        prop_assert_eq!(back.header_type, header_type);
        prop_assert_eq!(back.granule_position, granule_position);
        prop_assert_eq!(back.serial, serial);
        prop_assert_eq!(back.sequence, sequence);
        prop_assert_eq!(back.segments, segments);
        prop_assert_eq!(back.data, data);
    }

    #[test]
    fn pcm_i16_round_trip_within_one_step(samples in prop::collection::vec(-32768i16..=32767i16, 0..256)) {
        let channels = 1u32;
        let f32_samples: Vec<f32> = samples.iter().map(|s| f32::from(*s) / 32768.0).collect();
        let bytes = codec_whatttttttttttt_core::pcm::encode_samples(&f32_samples, PcmFormat::I16, channels).unwrap();
        let decoded = codec_whatttttttttttt_core::pcm::decode_samples(&bytes, PcmFormat::I16, channels).unwrap();
        prop_assert_eq!(decoded.len(), samples.len());
        for (original, back) in samples.iter().zip(decoded.iter()) {
            let expected = f32::from(*original) / 32768.0;
            prop_assert!((expected - back).abs() < 1.0 / 32768.0 + f32::EPSILON);
        }
    }
}
