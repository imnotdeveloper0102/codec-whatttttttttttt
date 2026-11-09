//! Internal benchmark for FLAC/Vorbis/DSD decode and FLAC encode.
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
    decode_flac_frame, encode_flac_stream, parse_flac_stream, scale_frame, Codec, Decoder,
    DsdDecimator, DsdDemuxer, Error, Frame, FramePool, OggDemuxer, PixelFormat, ScaleFilter,
    VideoFrame, VorbisSetup,
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

/// Collect all packets of a DSD fixture; returns the header
/// and the packets.
fn dsd_packets(
    bytes: Vec<u8>,
) -> Result<
    (
        codec_whatttttttttttt_core::DsdHeader,
        Vec<codec_whatttttttttttt_core::Packet>,
    ),
    Error,
> {
    let mut demuxer = DsdDemuxer::new(Cursor::new(bytes))?;
    let header = demuxer.header().clone();
    let mut packets = Vec::new();
    while let Some(packet) = demuxer.next_packet()? {
        packets.push(packet);
    }
    Ok((header, packets))
}

/// Decode a whole DSD fixture (packets already collected by the
/// caller) through a fresh decimator; returns the frame count.
/// The demuxing is done once by the caller, so the number
/// measures the decimation (and the rational resample for the
/// 48 kHz family), not the header parse or the file read.
fn dsd_decode_all(
    header: &codec_whatttttttttttt_core::DsdHeader,
    packets: &[codec_whatttttttttttt_core::Packet],
    target_rate: u32,
) -> Result<usize, Error> {
    let mut decimator = DsdDecimator::new(
        header.sample_rate,
        header.channels,
        target_rate,
        header.layout(),
    )?;
    let channels = header.channels as usize;
    let mut frames = 0usize;
    for packet in packets {
        frames += decimator.feed(&packet.data)?.len() / channels;
    }
    frames += decimator.flush()?.len() / channels;
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

/// DSD decode benchmarks: 0.1 s of stereo from the DSF
/// fixtures. The packets are collected once; only the
/// decimation is timed, with a fresh decimator (fresh filter
/// state) per iteration. The 192 kHz case exercises the 48 kHz
/// family: decimate to 705.6 kHz, then the rational resample
/// to 192 kHz.
#[allow(
    clippy::cast_precision_loss,
    reason = "bench fixtures hold at most a few hundred thousand frames, exactly representable in f64"
)]
fn bench_dsd() -> Result<(), Error> {
    for (name, target_rate) in [
        ("dsf_dsd128_stereo.dsf", 176_400u32),
        ("dsf_dsd512_stereo.dsf", 352_800),
        ("dsf_dsd256_stereo.dsf", 192_000),
    ] {
        let (header, packets) = dsd_packets(data(name)?)?;
        let frames = dsd_decode_all(&header, &packets, target_rate)?;
        let audio_seconds = frames as f64 / f64::from(target_rate);
        let time = median_time(|| dsd_decode_all(&header, &packets, target_rate).map(|_| ()))?;
        println!(
            "dsd {name:>24} -> {target_rate:>7} Hz {:>8.1} us  {:>6.1}x realtime  ({} frames, {}ch)",
            time.as_micros(),
            audio_seconds / time.as_secs_f64(),
            frames,
            header.channels,
        );
    }
    Ok(())
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

    // 8K scale + pool. A separate benchmark, NOT run in CI: the
    // `rust` job in .github/workflows/ci.yml runs fmt, clippy and
    // tests only, with no `cargo bench` step. Run it manually
    // with `cargo run --release -p codec-whatttttttttttt-core
    // --example bench`. Each filter is timed once (not the
    // median of 10) because one 8K resample already costs
    // hundreds of millions of tap evaluations. One 8K YUV 4:2:0
    // 8-bit frame is 49,766,400 bytes; the pool section reuses
    // that one buffer instead of reallocating it.
    let mut pool = FramePool::new(2);
    let mut frame_8k = pool.checkout(7680, 4320, PixelFormat::Yuv420, 8)?;
    for (index, value) in frame_8k.planes[0].iter_mut().enumerate() {
        *value = ((index % 7680) * 255 / 7680) as u8;
    }
    for filter in [
        ScaleFilter::Bilinear,
        ScaleFilter::Bicubic,
        ScaleFilter::Lanczos3,
    ] {
        let name = match filter {
            ScaleFilter::Bilinear => "bilinear",
            ScaleFilter::Bicubic => "bicubic",
            ScaleFilter::Lanczos3 => "lanczos3",
        };
        let start = Instant::now();
        let scaled: VideoFrame = scale_frame(&frame_8k, 3840, 2160, filter)?;
        let elapsed = start.elapsed();
        let pixels = u64::from(scaled.width) * u64::from(scaled.height);
        println!(
            "8k scale {name:>8} {:>8.1} ms  {:>6.1} MP/s out  (7680x4320 -> 3840x2160)",
            elapsed.as_millis(),
            pixels as f64 / elapsed.as_secs_f64() / 1_000_000.0,
        );
    }
    let pool_start = Instant::now();
    for _ in 0..10 {
        let frame = pool.checkout(7680, 4320, PixelFormat::Yuv420, 8)?;
        pool.checkin(frame);
    }
    let pool_elapsed = pool_start.elapsed();
    let stats = pool.stats();
    println!(
        "8k pool      {:>8.1} us for 10 checkout/checkin rounds  ({} allocations, {} reuses)",
        pool_elapsed.as_micros(),
        stats.allocations,
        stats.reuses,
    );
    pool.checkin(frame_8k);

    bench_dsd()?;
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
