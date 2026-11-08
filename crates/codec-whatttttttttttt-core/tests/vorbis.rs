//! Integration tests for Vorbis (Vorbis I specification).
//!
//! The reference vectors are real Vorbis files encoded with ffmpeg
//! 8.1.2 (libvorbis, quality 5) from the local CC0 WAV vectors; see
//! `tests/data/README.md` for the exact commands. The decoded PCM
//! is compared with the original WAV: Vorbis is lossy, so the
//! tolerance is the measured lossy difference (stereo RMS 0.0059,
//! max 0.016; mono RMS 0.0103, max 0.039), not bit-exactness.
//! Against the reference decoder (ffmpeg/libvorbis) the same output
//! matches to within 1e-6 (f32 rounding), measured outside this
//! test suite; see `CHANGELOG.md`.

use std::io::Cursor;

use codec_whatttttttttttt_core::{
    Codec, Decoder, Error, Frame, OggDemuxer, VorbisSetup, WavDemuxer,
};

fn stereo_bytes() -> Vec<u8> {
    include_bytes!("data/sine_stereo_44100_vorbis.ogg").to_vec()
}

fn mono_bytes() -> Vec<u8> {
    include_bytes!("data/sine_mono_8000_vorbis.ogg").to_vec()
}

/// The three header packets of a Vorbis file, in order.
fn headers(bytes: Vec<u8>) -> (Vec<u8>, Vec<u8>, Vec<u8>, OggDemuxer) {
    let mut demuxer = OggDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(demuxer.codec(), Codec::Vorbis);
    let identification = demuxer.next_packet().unwrap().unwrap();
    let comment = demuxer.next_packet().unwrap().unwrap();
    let setup = demuxer.next_packet().unwrap().unwrap();
    (identification.data, comment.data, setup.data, demuxer)
}

#[test]
fn all_three_headers_parse_from_real_stereo_file() {
    let (identification, comment, setup, _) = headers(stereo_bytes());
    let parsed = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap();
    assert_eq!(parsed.identification.channels, 2);
    assert_eq!(parsed.identification.sample_rate, 44100);
    assert_eq!(parsed.identification.blocksize_0, 256);
    assert_eq!(parsed.identification.blocksize_1, 2048);
    assert_eq!(parsed.comment.vendor, b"Lavf62.12.102");
    assert_eq!(parsed.comment.comments.len(), 1);
    assert_eq!(parsed.codebooks.len(), 42);
    assert_eq!(parsed.floors.len(), 2);
    assert_eq!(parsed.residues.len(), 2);
    assert_eq!(parsed.mappings.len(), 2);
    assert_eq!(parsed.modes.len(), 2);
}

#[test]
fn all_three_headers_parse_from_real_mono_file() {
    let (identification, comment, setup, _) = headers(mono_bytes());
    let parsed = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap();
    assert_eq!(parsed.identification.channels, 1);
    assert_eq!(parsed.identification.sample_rate, 8000);
    assert_eq!(parsed.identification.blocksize_0, 512);
    assert_eq!(parsed.identification.blocksize_1, 512);
    assert_eq!(parsed.codebooks.len(), 19);
    assert_eq!(parsed.floors.len(), 1);
    assert_eq!(parsed.residues.len(), 1);
    assert_eq!(parsed.mappings.len(), 1);
    assert_eq!(parsed.modes.len(), 1);
}

#[test]
fn bad_setup_header_returns_invalid_data() {
    let (identification, comment, mut setup, _) = headers(stereo_bytes());
    // Corrupt the setup header's packet type (0x05 -> 0x06).
    setup[0] = 0x06;
    let err = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn bad_identification_version_returns_invalid_data() {
    let (mut identification, comment, setup, _) = headers(stereo_bytes());
    // Identification version is a u32 at bytes 7..11; set it to 1.
    identification[7] = 1;
    let err = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap_err();
    assert!(matches!(err, Error::InvalidData { .. }));
}

#[test]
fn truncated_setup_header_returns_truncated() {
    let (identification, comment, setup, _) = headers(stereo_bytes());
    // Cut the setup header in half; the codebook parse runs out
    // of bits.
    let cut = setup.len() / 2;
    let err = VorbisSetup::from_headers(&identification, &comment, &setup[..cut]).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

#[test]
fn truncated_audio_packet_returns_truncated() {
    let (identification, comment, setup, mut demuxer) = headers(stereo_bytes());
    let parsed = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap();
    let mut decoder = Decoder::from_vorbis_setup(parsed).unwrap();
    // An empty audio packet has no bits for the packet-type read.
    // (A merely short packet does not error: end-of-packet during
    // floor and residue decode is nominal per the spec, sections
    // 4.3.2 and 8.6.2, and yields partial silence.)
    let packet = demuxer.next_packet().unwrap().unwrap();
    let mut truncated = packet.clone();
    truncated.data.clear();
    let err = decoder.decode(&truncated).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }));
}

/// Decodes a whole Vorbis file and returns all samples, including
/// the flushed tail.
fn decode_all(bytes: Vec<u8>) -> (Vec<f32>, u32, u32) {
    let (identification, comment, setup, mut demuxer) = headers(bytes);
    let parsed = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap();
    let channels = parsed.identification.channels;
    let sample_rate = parsed.identification.sample_rate;
    let mut decoder = Decoder::from_vorbis_setup(parsed).unwrap();
    let mut samples = Vec::new();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        match decoder.decode(&packet).unwrap() {
            Frame::Audio(audio) => samples.extend_from_slice(&audio.samples),
            Frame::Video(_) => panic!("expected an audio frame"),
        }
    }
    if let Some(Frame::Audio(tail)) = decoder.flush().unwrap() {
        samples.extend_from_slice(&tail.samples);
    }
    assert_eq!(decoder.flush().unwrap(), None);
    (samples, channels, sample_rate)
}

// `n` is a sample count of at most a few thousand, exact in f64.
#[allow(clippy::cast_precision_loss)]
fn rms_and_max(a: &[f32], b: &[f32]) -> (f64, f32) {
    let n = a.len().min(b.len());
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for i in 0..n {
        let diff = f64::from(a[i]) - f64::from(b[i]);
        sum += diff * diff;
        max = max.max((a[i] - b[i]).abs());
    }
    ((sum / n as f64).sqrt(), max)
}

#[test]
fn stereo_pcm_matches_original_wav_within_lossy_tolerance() {
    let (samples, channels, sample_rate) = decode_all(stereo_bytes());
    assert_eq!(channels, 2);
    assert_eq!(sample_rate, 44100);
    // 5824 frames: the priming block and the flushed tail are
    // included; the first 4410 frames align with the original.
    assert_eq!(samples.len(), 5824 * 2);
    let wav = include_bytes!("data/valid_i16_stereo_44100.wav");
    let original = WavDemuxer::new(Cursor::new(wav.to_vec()))
        .unwrap()
        .samples()
        .unwrap();
    let (rms, max) = rms_and_max(&samples[..original.len()], &original);
    // Measured on the reference vectors (libvorbis q5): RMS
    // 0.0059, max 0.016. The bound leaves headroom for the lossy
    // codec difference; the decoder itself matches libvorbis to
    // within 1e-6.
    assert!(rms < 0.02, "rms {rms}");
    assert!(max < 0.05, "max {max}");
}

#[test]
fn mono_pcm_matches_original_wav_within_lossy_tolerance() {
    let (samples, channels, sample_rate) = decode_all(mono_bytes());
    assert_eq!(channels, 1);
    assert_eq!(sample_rate, 8000);
    assert_eq!(samples.len(), 1280);
    let wav = include_bytes!("data/valid_i16_mono_8000.wav");
    let original = WavDemuxer::new(Cursor::new(wav.to_vec()))
        .unwrap()
        .samples()
        .unwrap();
    let (rms, max) = rms_and_max(&samples[..original.len()], &original);
    // Measured on the reference vectors (libvorbis q5): RMS
    // 0.0103, max 0.039.
    assert!(rms < 0.02, "rms {rms}");
    assert!(max < 0.05, "max {max}");
}

#[test]
fn noise_and_ramp_files_decode_to_non_silent_audio() {
    // The noise and ramp vectors exercise different residue paths
    // (verbatim-like spectra and LPC floors); they must decode
    // without error and produce real (non-silent) audio.
    for bytes in [
        include_bytes!("data/noise_mono_8000_vorbis.ogg").to_vec(),
        include_bytes!("data/ramp_mono_lpc_vorbis.ogg").to_vec(),
    ] {
        let (samples, got_channels, got_rate) = decode_all(bytes);
        assert_eq!(got_channels, 1);
        assert_eq!(got_rate, 8000);
        assert!(samples.iter().any(|&s| s.abs() > 0.01));
    }
}

#[test]
fn flush_returns_tail_once_then_none() {
    let (identification, comment, setup, mut demuxer) = headers(mono_bytes());
    let parsed = VorbisSetup::from_headers(&identification, &comment, &setup).unwrap();
    let mut decoder = Decoder::from_vorbis_setup(parsed).unwrap();
    while let Some(packet) = demuxer.next_packet().unwrap() {
        decoder.decode(&packet).unwrap();
    }
    let first = decoder.flush().unwrap();
    assert!(matches!(first, Some(Frame::Audio(_))));
    assert_eq!(decoder.flush().unwrap(), None);
}
