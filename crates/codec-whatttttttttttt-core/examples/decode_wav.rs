//! Decodes a WAV file and prints its header plus the first samples.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p codec-whatttttttttttt-core --example decode_wav -- \
//!   crates/codec-whatttttttttttt-core/tests/data/valid_i16_stereo_44100.wav
//! ```

use std::env;
use std::fs::File;
use std::process::ExitCode;

use codec_whatttttttttttt_core::{Decoder, Frame, WavDemuxer};

fn run(path: &str) -> Result<(), codec_whatttttttttttt_core::Error> {
    let file = File::open(path)?;
    let mut demuxer = WavDemuxer::new(file)?;
    let header = *demuxer.header();
    println!("format: {:?}", header.format);
    println!("channels: {}", header.channels);
    println!("sample_rate: {} Hz", header.sample_rate);
    println!("frames: {}", header.frame_count());
    let mut decoder = Decoder::from_wav_header(&header)?;
    while let Some(packet) = demuxer.next_packet()? {
        let frame = decoder.decode(&packet)?;
        if let Frame::Audio(audio) = frame {
            let shown = audio.samples.iter().take(8).collect::<Vec<_>>();
            println!("first samples: {shown:?}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: decode_wav <input.wav>");
        return ExitCode::FAILURE;
    };
    match run(path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
