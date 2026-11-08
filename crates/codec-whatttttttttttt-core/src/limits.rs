//! Hard limits, enforced at the trust boundary where external data enters.
//!
//! A value that exceeds a limit is rejected with `AllocationLimit` or
//! `Overflow`; the library never allocates the declared size and never
//! returns a truncated frame instead. All limits live here so a reviewer
//! can audit them in one place.

/// Largest audio packet accepted by `Decoder::decode` (1 MiB).
pub const MAX_AUDIO_PACKET_BYTES: usize = 1024 * 1024;

/// Largest video packet accepted by `Decoder::decode` (16 MiB).
pub const MAX_VIDEO_PACKET_BYTES: usize = 16 * 1024 * 1024;

/// Largest number of audio channels (FLAC/Opus/Vorbis maxima are within this).
pub const MAX_CHANNELS: u32 = 8;

/// Largest sample rate in Hz (FLAC maximum, RFC 9639).
pub const MAX_SAMPLE_RATE: u32 = 192_000;

/// Largest video width or height in pixels (above 8K is out of scope).
pub const MAX_WIDTH: u32 = 8192;
/// Largest video height in pixels.
pub const MAX_HEIGHT: u32 = 8192;

/// Largest metadata block accepted (1 MiB).
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;

/// Largest box/element nesting depth for MP4/EBML traversal.
pub const MAX_NESTING_DEPTH: u32 = 32;

/// Largest Huffman code length accepted.
pub const MAX_HUFFMAN_CODE_LENGTH: u32 = 32;

/// Largest total allocation for one decode call (64 MiB).
pub const MAX_ALLOCATION_BYTES: usize = 64 * 1024 * 1024;

/// Largest WebM/Matroska file accepted by `MatroskaDemuxer` (512 MiB).
pub const MAX_WEBM_FILE_BYTES: usize = 512 * 1024 * 1024;

/// Largest MP4 file accepted by `Mp4Demuxer` (512 MiB).
pub const MAX_MP4_FILE_BYTES: usize = 512 * 1024 * 1024;

/// Largest number of tracks in one WebM/MP4 file.
pub const MAX_TRACKS: usize = 64;

/// Largest number of samples in one MP4 track. Each sample entry is
/// small, but an attacker-controlled `stsz` count must not drive an
/// unbounded allocation; the sample table is also bounded by the
/// file size, since every sample needs at least one byte in `mdat`.
pub const MAX_SAMPLES_PER_TRACK: usize = 1_000_000;

/// Largest WAV file accepted by `WavDemuxer` (512 MiB). WAV is an
/// uncompressed container, so a whole-file bound larger than the per-frame
/// bound is needed; streaming demux in a later phase removes this cap.
pub const MAX_WAV_FILE_BYTES: usize = 512 * 1024 * 1024;

/// Largest FLAC file accepted by `FlacDemuxer` (512 MiB).
pub const MAX_FLAC_FILE_BYTES: usize = 512 * 1024 * 1024;

/// Largest Ogg file accepted by `OggDemuxer` (512 MiB).
pub const MAX_OGG_FILE_BYTES: usize = 512 * 1024 * 1024;

/// Chunk size used by `BoundedReader` (64 KiB).
pub const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Largest number of samples in one decoded audio frame, derived from the
/// per-call allocation limit for f32 samples.
pub const MAX_FRAME_SAMPLES: usize = MAX_ALLOCATION_BYTES / 4;

// Compile-time consistency checks. A plain test would assert on
// constants, which clippy rejects; anonymous const blocks run the
// same checks at compile time.
const _: () = {
    assert!(MAX_AUDIO_PACKET_BYTES <= MAX_ALLOCATION_BYTES);
    assert!(MAX_VIDEO_PACKET_BYTES <= MAX_ALLOCATION_BYTES);
    assert!(MAX_METADATA_BYTES <= MAX_ALLOCATION_BYTES);
    assert!(MAX_CHANNELS > 0);
    assert!(MAX_SAMPLE_RATE > 0);
    assert!(READ_CHUNK_BYTES > 0);
};
