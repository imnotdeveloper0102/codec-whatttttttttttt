//! Internal benchmark for FLAC/Vorbis decode and FLAC encode.
//!
//! Not criterion: a plain binary using `Instant`, so the numbers
//! come from this machine with no extra dependency. The fixtures
//! are the pinned files in `tests/data/`; the encode input is
//! generated (5 s of stereo 44.1 kHz sine), so it measures the
//! encoder, not the disk.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run --release -p codec-whatttttttttttt-core --example bench
//! ```
//!
//! Each benchmark warms up 3 times, then runs 10 timed
//! iterations and reports the median. "x realtime" is audio
//! duration divided by decode time: above 1 means faster than
//! realtime.

use std::io::Cursor;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use codec_whatttttttttttt_core::{
    decode_flac_frame, encode_flac_stream, parse_flac_stream, Codec, Decoder, Error, Frame,
    OggDemuxer, VorbisSetup,
};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");

fn data(name: &str) -> Result<Vec<u8>, Error> {
    std::fs::read(format!("{DATA}/{name}")).map_err(Error::from)
}

/// The three header packets of a Vorbis file, in order, plus the
/// demuxer positioned after them.
type VorbisHeaders = (Vec<u8>, Vec<u8>, Vec<u8>, OggDemuxer);

fn vorbis_headers(bytes: Vec<u8>) -> Result<VorbisHeaders, Error> {
    let mut demuxer = OggDemuxer::new(Cursor::new(bytes))?;
    if demuxer.codec() != Codec::Vorbis {
        return Err(Error::InvalidData {
            offset: 0,
            detail: "expected a vorbis stream",
        });
    }
    let identification = demuxer
        .next_packet()?
        .ok_or(Error::Truncated { needed: 1, had: 0 })?;
    let comment = demuxer
        .next_packet()?
        .ok_or(Error::Truncated { needed: 1, had: 0 })?;
    let setup = demuxer
        .next_packet()?
        .ok_or(Error::Truncated { needed: 1, had: 0 })?;
    Ok((identification.data, comment.data, setup.data, demuxer))
}

/// Decode a whole FLAC file; returns (frames, channels, sample rate).
/// The stream is parsed once by the caller; only frame decoding is
/// timed, so the number measures the decoder, not the header parse.
fn flac_decode_all(
    bytes: &[u8],
    stream: &codec_whatttttttttttt_core::FlacStream,
) -> Result<u64, Error> {
    let mut frames = 0u64;
    let mut offset = stream.first_frame_offset;
    while offset < bytes.len() {
        let (frame, consumed) = decode_flac_frame(&bytes[offset..], &stream.streaminfo)?;
        if consumed == 0 {
            return Err(Error::InvalidData {
                offset,
                detail: "flac frame consumed zero bytes",
            });
        }
        frames += frame.frame_count() as u64;
        offset += consumed;
    }
    Ok(frames)
}

/// Decode a whole Vorbis file from already-parsed setup and
/// packets; returns the frame count. The setup is parsed once by
/// the caller and cloned per iteration, so the number measures
/// packet decoding (including the MDCT), not header parsing.
fn vorbis_decode_all(
    setup: &VorbisSetup,
    packets: &[codec_whatttttttttttt_core::Packet],
) -> Result<u64, Error> {
    let mut decoder = Decoder::from_vorbis_setup(setup.clone())?;
    let mut frames = 0u64;
    for packet in packets {
        match decoder.decode(packet)? {
            Frame::Audio(audio) => frames += audio.frame_count() as u64,
            Frame::Video(_) => {
                return Err(Error::InvalidData {
                    offset: 0,
                    detail: "expected an audio frame",
                });
            }
        }
    }
    if let Some(Frame::Audio(tail)) = decoder.flush()? {
        frames += tail.frame_count() as u64;
    }
    Ok(frames)
}

/// Run `run` 3 warmup times, then 10 timed times; return the
/// median duration.
fn median_time<F: FnMut() -> Result<(), Error>>(mut run: F) -> Result<Duration, Error> {
    for _ in 0..3 {
        run()?;
    }
    let mut times: Vec<Duration> = Vec::with_capacity(10);
    for _ in 0..10 {
        let start = Instant::now();
        run()?;
        times.push(start.elapsed());
    }
    times.sort_unstable();
    Ok(times[5])
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bench fixtures hold at most a few hundred thousand frames, exactly representable in f64"
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "sine amplitudes are bounded to 20000, inside the i16 range"
)]
fn run() -> Result<(), Error> {
    // FLAC decode: 4410 frames of stereo 44.1 kHz (0.1 s of audio).
    let flac = data("sine_stereo_44100.flac")?;
    let stream = parse_flac_stream(&flac)?;
    let frames = flac_decode_all(&flac, &stream)?;
    let rate = stream.streaminfo.sample_rate;
    let channels = stream.streaminfo.channels;
    let audio_seconds = frames as f64 / f64::from(rate);
    let flac_time = median_time(|| flac_decode_all(&flac, &stream).map(|_| ()))?;
    println!(
        "flac decode   {:>8.1} us  {:>6.1}x realtime  ({} frames, {}ch, {} Hz)",
        flac_time.as_micros(),
        audio_seconds / flac_time.as_secs_f64(),
        frames,
        channels,
        rate,
    );

    // Vorbis decode: the same sine as Vorbis in Ogg. Headers and
    // packets are collected once; only packet decoding is timed.
    let vorbis = data("sine_stereo_44100_vorbis.ogg")?;
    let (identification, comment, setup_bytes, mut demuxer) = vorbis_headers(vorbis)?;
    let setup = VorbisSetup::from_headers(&identification, &comment, &setup_bytes)?;
    let channels = setup.identification.channels;
    let rate = setup.identification.sample_rate;
    let mut packets = Vec::new();
    while let Some(packet) = demuxer.next_packet()? {
        packets.push(packet);
    }
    let frames = vorbis_decode_all(&setup, &packets)?;
    let audio_seconds = frames as f64 / f64::from(rate);
    let vorbis_time = median_time(|| vorbis_decode_all(&setup, &packets).map(|_| ()))?;
    println!(
        "vorbis decode {:>8.1} us  {:>6.1}x realtime  ({} frames, {}ch, {} Hz)",
        vorbis_time.as_micros(),
        audio_seconds / vorbis_time.as_secs_f64(),
        frames,
        channels,
        rate,
    );

    // FLAC encode: 5 s of stereo 44.1 kHz sine, generated.
    let encode_frames = 44100 * 5;
    let mut samples = Vec::with_capacity(encode_frames * 2);
    for n in 0..encode_frames {
        let t = n as f64 / 44100.0;
        samples.push((20000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16);
        samples.push((15000.0 * (2.0 * std::f64::consts::PI * 660.0 * t).sin()) as i16);
    }
    let encoded = encode_flac_stream(&samples, 2, 44100)?;
    let input_bytes = samples.len() * 2;
    let encode_time = median_time(|| encode_flac_stream(&samples, 2, 44100).map(|_| ()))?;
    println!(
        "flac encode   {:>8.1} us  {:>6.1} MB/s in, {} bytes out  ({} frames, 2ch, 44100 Hz)",
        encode_time.as_micros(),
        input_bytes as f64 / encode_time.as_secs_f64() / 1_000_000.0,
        encoded.len(),
        encode_frames,
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bench failed: {error}");
            ExitCode::FAILURE
        }
    }
}
