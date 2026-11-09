//! Integration tests for Vorbis floor type 0 packet decode
//! (Vorbis I specification sections 6.2.2 and 6.2.3).
//!
//! No encoder since libvorbis beta 4 emits floor 0, and ffmpeg's
//! libvorbis encoder has no floor 0 option, so there is no real
//! floor 0 file to test against: the stream below is synthesized
//! by hand in this file (three header packets plus audio packets,
//! all written LSB-first with the local bit writer). The setup is
//! minimal on purpose: mono, 8000 Hz, blocksize 256, one floor 0
//! (order 2, rate 8000, Bark map 64, amplitude bits 8, amplitude
//! offset 10, one VQ book), one format-1 residue whose VQ book
//! returns [1.0, 1.0] for every partition, so the decoded spectrum
//! is the floor curve itself and the expected output is computed
//! from the spec formula with an independent `f64` reference
//! implementation in this file. The exact curve values are pinned
//! separately by the unit tests in `vorbis_setup.rs`, which check
//! them against values computed by hand.

use codec_whatttttttttttt_core::{mdct_inverse, VorbisPacketDecoder, VorbisSetup};

/// Packs bits LSB-first into bytes, mirroring `LsbBitReader`.
struct LsbWriter {
    bytes: Vec<u8>,
    bit_pos: usize,
}

impl LsbWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit_pos: 0,
        }
    }

    fn write_bits(&mut self, value: u32, n: u32) {
        for i in 0..n {
            let bit = (value >> i) & 1;
            if self.bit_pos % 8 == 0 {
                self.bytes.push(0);
            }
            if bit == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 1 << (self.bit_pos % 8);
            }
            self.bit_pos += 1;
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_bits(u32::from(byte), 8);
        }
    }

    /// Writes a Huffman codeword: the bits are written in stream
    /// order, most significant bit first, unlike the numeric
    /// fields above which are LSB-first.
    fn write_codeword(&mut self, code: u32, len: u32) {
        for i in (0..len).rev() {
            self.write_bits((code >> i) & 1, 1);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Writes one lookup-type-2 codebook into `writer`: dimensions 2,
/// two entries, codeword lengths [1, 1] (entry 0 is the code `0`,
/// entry 1 is `1`), minimum 0.0, delta 1.0, value bits 3,
/// `sequence_p` 0, and the given multiplicands (four values: two
/// per entry).
fn write_vq_book(writer: &mut LsbWriter, multiplicands: [u32; 4]) {
    writer.write_bytes(&[0x42, 0x43, 0x56]);
    writer.write_bits(2, 16); // dimensions
    writer.write_bits(2, 24); // entries
    writer.write_bits(0, 1); // unordered
    writer.write_bits(0, 1); // not sparse
    writer.write_bits(0, 5); // length 1
    writer.write_bits(0, 5); // length 1
    writer.write_bits(2, 4); // lookup type 2
    writer.write_bits(0, 32); // minimum 0.0
    writer.write_bits(0x6010_0000, 32); // delta 1.0
    writer.write_bits(2, 4); // value_bits 3
    writer.write_bits(0, 1); // sequence_p 0
    for &multiplicand in &multiplicands {
        writer.write_bits(multiplicand, 3);
    }
}

/// Writes a single-entry codebook for scalar-only use (the
/// residue classbook): dimensions 1, one entry, codeword length 1,
/// lookup type 0. `decode_scalar` sinks one bit and returns 0.
fn write_classbook(writer: &mut LsbWriter) {
    writer.write_bytes(&[0x42, 0x43, 0x56]);
    writer.write_bits(1, 16); // dimensions
    writer.write_bits(1, 24); // entries
    writer.write_bits(0, 1); // unordered
    writer.write_bits(0, 1); // not sparse
    writer.write_bits(0, 5); // length 1
    writer.write_bits(0, 4); // lookup type 0
}

/// The three header packets of the synthetic floor 0 stream.
fn headers_with_amplitude_bits(amplitude_bits: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // Identification: mono, 8000 Hz, blocksize 256/256.
    let mut identification = LsbWriter::new();
    identification.write_bytes(b"\x01vorbis");
    identification.write_bits(0, 32); // version
    identification.write_bits(1, 8); // channels
    identification.write_bits(8000, 32); // sample rate
    identification.write_bits(0, 32); // bitrate maximum
    identification.write_bits(0, 32); // bitrate nominal
    identification.write_bits(0, 32); // bitrate minimum
    identification.write_bits(8, 4); // blocksize_0 = 2^8
    identification.write_bits(8, 4); // blocksize_1 = 2^8
    identification.write_bits(1, 1); // framing bit
    let identification = identification.finish();

    // Comment: vendor "test", no user comments.
    let mut comment = LsbWriter::new();
    comment.write_bytes(b"\x03vorbis");
    comment.write_bits(4, 32); // vendor length
    comment.write_bytes(b"test");
    comment.write_bits(0, 32); // comment count
    comment.write_bits(1, 1); // framing bit
    let comment = comment.finish();

    // Setup: 3 codebooks, 1 time placeholder, 1 floor 0,
    // 1 residue (format 1), 1 mapping, 1 mode.
    let mut setup = LsbWriter::new();
    setup.write_bytes(b"\x05vorbis");
    setup.write_bits(2, 8); // codebook count 3
    write_vq_book(&mut setup, [1, 1, 2, 2]);
    write_classbook(&mut setup);
    write_vq_book(&mut setup, [1, 1, 1, 1]);
    setup.write_bits(0, 6); // time count 1
    setup.write_bits(0, 16); // time placeholder
    setup.write_bits(0, 6); // floor count 1
    setup.write_bits(0, 16); // floor type 0
    setup.write_bits(2, 8); // order
    setup.write_bits(8000, 16); // rate
    setup.write_bits(64, 16); // bark map size
    setup.write_bits(u32::from(amplitude_bits), 6); // amplitude bits
    setup.write_bits(10, 8); // amplitude offset
    setup.write_bits(0, 4); // book count 1
    setup.write_bits(0, 8); // book_list[0] = book 0
    setup.write_bits(0, 6); // residue count 1
    setup.write_bits(1, 16); // residue type 1
    setup.write_bits(0, 24); // begin
    setup.write_bits(128, 24); // end
    setup.write_bits(1, 24); // partition size 2
    setup.write_bits(0, 6); // classifications 1
    setup.write_bits(1, 8); // classbook 1
    setup.write_bits(1, 3); // cascade low bits: pass 0 only
    setup.write_bits(0, 1); // cascade bitflag: no high bits
    setup.write_bits(2, 8); // books[0][0] = book 2
    setup.write_bits(0, 6); // mapping count 1
    setup.write_bits(0, 16); // mapping type 0
    setup.write_bits(0, 1); // one submap
    setup.write_bits(0, 1); // no coupling
    setup.write_bits(0, 2); // reserved
    setup.write_bits(0, 8); // submap time placeholder
    setup.write_bits(0, 8); // submap floor 0
    setup.write_bits(0, 8); // submap residue 0
    setup.write_bits(0, 6); // mode count 1
    setup.write_bits(0, 1); // blockflag: short block
    setup.write_bits(0, 16); // window type
    setup.write_bits(0, 16); // transform type
    setup.write_bits(0, 8); // mapping 0
    setup.write_bits(1, 1); // framing bit
    let setup = setup.finish();

    (identification, comment, setup)
}

fn setup_with_amplitude_bits(amplitude_bits: u8) -> VorbisSetup {
    let (identification, comment, setup) = headers_with_amplitude_bits(amplitude_bits);
    VorbisSetup::from_headers(&identification, &comment, &setup).unwrap()
}

fn setup() -> VorbisSetup {
    setup_with_amplitude_bits(8)
}

/// One audio packet for the synthetic stream: type bit 0, no mode
/// bits (one mode), no window flags (short block), then the floor
/// 0 payload with the given amplitude and VQ entry, then `residue`
/// zero bits for the residue (one class bit plus one VQ codeword
/// bit per partition, 64 partitions).
fn audio_packet(amplitude: u32, vq_entry: u32, residue_zero_bits: usize) -> Vec<u8> {
    let mut packet = LsbWriter::new();
    packet.write_bits(0, 1); // audio packet
    packet.write_bits(amplitude, 8);
    if amplitude != 0 {
        packet.write_codeword(vq_entry, 1);
    }
    for _ in 0..residue_zero_bits {
        packet.write_bits(0, 1);
    }
    packet.finish()
}

/// Bark scale (Vorbis I spec section 6.2.3, post-errata 20150227).
fn bark(x: f64) -> f64 {
    13.1 * (0.00074 * x).atan() + 2.24 * (0.000_000_018_5 * x * x).atan() + 0.0001 * x
}

/// Parameters of the floor 0 curve used by the reference
/// implementation (bundled so the function stays under the
/// argument-count lint).
struct CurveParams {
    order: usize,
    rate: u16,
    bark_map_size: u16,
    amplitude_bits: u8,
    amplitude_offset: u8,
    amplitude: u16,
    n: usize,
}

/// Independent `f64` reference implementation of the floor 0
/// curve (spec section 6.2.3), written from the spec text.
/// `coefficients` are the decoded LSP angles in radians.
// The casts are bounded: the map value is floored and clamped to
// `[0, bark_map_size - 1]` (at most 65535) before the `f64` to
// integer conversion, the indices are block sizes (at most 8192),
// and the curve value is the output format's own `f32` precision.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn reference_curve(params: &CurveParams, coefficients: &[f64]) -> Vec<f32> {
    let CurveParams {
        order,
        rate,
        bark_map_size,
        amplitude_bits,
        amplitude_offset,
        amplitude,
        n,
    } = *params;
    let bark_map_size_f = f64::from(bark_map_size);
    let bark_max = bark(f64::from(rate) * 0.5);
    let map: Vec<u32> = (0..n)
        .map(|i| {
            let frequency = f64::from(rate) * i as f64 / (2.0 * n as f64);
            let value = (bark(frequency) * bark_map_size_f / bark_max).floor() as i64;
            u32::try_from(value.clamp(0, i64::from(bark_map_size) - 1)).unwrap_or(0)
        })
        .collect();
    let cos_coefficients: Vec<f64> = coefficients.iter().map(|&value| value.cos()).collect();
    let maxval = 2f64.powi(i32::from(amplitude_bits)) - 1.0;
    let mut output = vec![0.0f32; n];
    let mut i = 0usize;
    while i < n {
        let omega = std::f64::consts::PI * f64::from(map[i]) / bark_map_size_f;
        let cos_omega = omega.cos();
        let (p, q) = if order % 2 == 1 {
            let mut p = 1.0 - cos_omega * cos_omega;
            for j in 0..(order - 1) / 2 {
                let difference = cos_coefficients[2 * j + 1] - cos_omega;
                p *= 4.0 * difference * difference;
            }
            let mut q = 0.25;
            for j in 0..order.div_ceil(2) {
                let difference = cos_coefficients[2 * j] - cos_omega;
                q *= 4.0 * difference * difference;
            }
            (p, q)
        } else {
            let mut p = (1.0 - cos_omega) / 2.0;
            let mut q = (1.0 + cos_omega) / 2.0;
            for j in 0..order / 2 {
                let difference = cos_coefficients[2 * j + 1] - cos_omega;
                p *= 4.0 * difference * difference;
                let difference = cos_coefficients[2 * j] - cos_omega;
                q *= 4.0 * difference * difference;
            }
            (p, q)
        };
        let linear = (0.115_129_25
            * (f64::from(amplitude) * f64::from(amplitude_offset) / (maxval * (p + q).sqrt())
                - f64::from(amplitude_offset)))
        .exp();
        let value = linear as f32;
        let condition = map[i];
        while i < n && map[i] == condition {
            output[i] = value;
            i += 1;
        }
    }
    output
}

/// The short-block window (spec sections 1.3.2 and 4.3.1): two
/// slopes meeting at the middle, no flat region.
fn short_window(n: usize) -> Vec<f32> {
    let mut window = vec![0.0f32; n];
    let half = n / 2;
    // Window indices are below the block size (at most 8192),
    // so the conversion via `u16` to `f32` is exact.
    let half_f = f32::from(u16::try_from(half).unwrap_or(0));
    for (rel, w) in window[..half].iter_mut().enumerate() {
        let rel_f = f32::from(u16::try_from(rel).unwrap_or(0));
        let theta = (rel_f + 0.5) / half_f * std::f32::consts::FRAC_PI_2;
        *w = (0.5 * std::f32::consts::PI * theta.sin().powi(2)).sin();
    }
    for (rel, w) in window[half..].iter_mut().enumerate() {
        let rel_f = f32::from(u16::try_from(rel).unwrap_or(0));
        let theta = (rel_f + 0.5) / half_f * std::f32::consts::FRAC_PI_2;
        *w = (0.5 * std::f32::consts::PI * theta.cos().powi(2)).sin();
    }
    window
}

/// Largest absolute difference between two sample vectors.
fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

#[test]
fn floor0_setup_headers_parse() {
    let parsed = setup();
    assert_eq!(parsed.identification.channels, 1);
    assert_eq!(parsed.identification.sample_rate, 8000);
    assert_eq!(parsed.identification.blocksize_0, 256);
    assert_eq!(parsed.identification.blocksize_1, 256);
    assert_eq!(parsed.codebooks.len(), 3);
    assert_eq!(parsed.floors.len(), 1);
    assert_eq!(parsed.residues.len(), 1);
    assert_eq!(parsed.mappings.len(), 1);
    assert_eq!(parsed.modes.len(), 1);
}

#[test]
fn floor0_packet_decodes_end_to_end_with_unit_residue() {
    // Two identical packets: the first primes the decoder, the
    // second yields the overlap-add of two identical blocks.
    // The residue is all ones, so the spectrum is the floor
    // curve; the expected block is the inverse MDCT of the
    // reference curve, and the expected output is the second
    // half of one block plus the first half of the next.
    let mut decoder = VorbisPacketDecoder::new(setup());
    let packet = audio_packet(255, 0, 128);
    assert!(decoder.decode_packet(&packet).unwrap().is_none());
    let output = decoder.decode_packet(&packet).unwrap().unwrap();
    assert_eq!(output.channels, 1);
    assert_eq!(output.sample_rate, 8000);
    assert_eq!(output.blocksize, 256);
    assert_eq!(output.samples.len(), 128);

    let curve = reference_curve(
        &CurveParams {
            order: 2,
            rate: 8000,
            bark_map_size: 64,
            amplitude_bits: 8,
            amplitude_offset: 10,
            amplitude: 255,
            n: 128,
        },
        &[1.0, 1.0],
    );
    let window = short_window(256);
    let mut block = vec![0.0f32; 256];
    mdct_inverse(&curve, &mut block, &window).unwrap();
    let mut expected = vec![0.0f32; 128];
    for i in 0..128 {
        expected[i] = block[128 + i] + block[i];
    }
    let diff = max_abs_diff(&output.samples, &expected);
    println!("floor 0 end-to-end max abs diff: {diff}");
    assert!(
        diff < 1e-5,
        "floor 0 output differs from the spec reference by {diff}"
    );
}

#[test]
fn floor0_zero_amplitude_packet_is_unused() {
    // Two amplitude-0 packets: the floor is unused, the residue
    // is skipped, and the output is all zeros.
    let mut decoder = VorbisPacketDecoder::new(setup());
    let packet = audio_packet(0, 0, 0);
    assert!(decoder.decode_packet(&packet).unwrap().is_none());
    let output = decoder.decode_packet(&packet).unwrap().unwrap();
    assert_eq!(output.samples.len(), 128);
    assert!(output.samples.iter().all(|&sample| sample == 0.0));
}

#[test]
fn floor0_packet_after_unused_prime_yields_first_half() {
    // Prime with an unused floor, then a floor 0 packet: the
    // previous block is all zeros, so the output is the first
    // half of the inverse-MDCT block of the reference curve.
    let mut decoder = VorbisPacketDecoder::new(setup());
    let unused = audio_packet(0, 0, 0);
    assert!(decoder.decode_packet(&unused).unwrap().is_none());
    let packet = audio_packet(255, 0, 128);
    let output = decoder.decode_packet(&packet).unwrap().unwrap();
    assert_eq!(output.samples.len(), 128);

    let curve = reference_curve(
        &CurveParams {
            order: 2,
            rate: 8000,
            bark_map_size: 64,
            amplitude_bits: 8,
            amplitude_offset: 10,
            amplitude: 255,
            n: 128,
        },
        &[1.0, 1.0],
    );
    let window = short_window(256);
    let mut block = vec![0.0f32; 256];
    mdct_inverse(&curve, &mut block, &window).unwrap();
    let diff = max_abs_diff(&output.samples, &block[..128]);
    println!("floor 0 first-half max abs diff: {diff}");
    assert!(
        diff < 1e-5,
        "floor 0 output differs from the spec reference by {diff}"
    );
}

#[test]
fn floor0_truncated_packet_is_unused_not_an_error() {
    // Amplitude is present but the packet ends before the VQ
    // vector: nominal end-of-packet, so the floor is unused
    // and the decode succeeds with all-zero output. This setup
    // uses 7 amplitude bits so the packet is exactly one byte
    // (1 type bit + 7 amplitude bits): with 8 amplitude bits
    // the byte padding would supply the VQ codeword bit and
    // the packet would not actually be truncated.
    let mut decoder = VorbisPacketDecoder::new(setup_with_amplitude_bits(7));
    let mut packet = LsbWriter::new();
    packet.write_bits(0, 1); // audio packet
    packet.write_bits(127, 7); // amplitude, then end of packet
    let packet = packet.finish();
    assert_eq!(packet.len(), 1);
    assert!(decoder.decode_packet(&packet).unwrap().is_none());
    let output = decoder.decode_packet(&packet).unwrap().unwrap();
    assert_eq!(output.samples.len(), 128);
    assert!(output.samples.iter().all(|&sample| sample == 0.0));
}
