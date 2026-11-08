//! Frame, packet, and metadata types shared by every codec.
//!
//! These types are the only data that crosses the decoder/encoder
//! boundary. A `Packet` is compressed data with a timestamp; a `Frame`
//! is decoded data. No stage passes raw bytes with an implicit format.

/// Codec selected by probing or by explicit construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Raw PCM samples, carried in WAV in Phase 1.
    Pcm,
    /// FLAC, RFC 9639 (decode from Phase 2).
    Flac,
    /// Opus, RFC 6716 (decode from Phase 4).
    Opus,
    /// Vorbis I (decode from Phase 5).
    Vorbis,
    /// AV1 (decode subset from Phase 7).
    Av1,
    /// VP9 (decode subset from Phase 8).
    Vp9,
}

impl Codec {
    /// Short name used in errors and logs.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Pcm => "pcm",
            Self::Flac => "flac",
            Self::Opus => "opus",
            Self::Vorbis => "vorbis",
            Self::Av1 => "av1",
            Self::Vp9 => "vp9",
        }
    }
}

/// Pixel layout of a decoded video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Planar YUV 4:2:0, three planes in Y, U, V order.
    Yuv420,
    /// Packed RGB, 3 bytes per pixel.
    Rgb24,
    /// Packed RGBA, 4 bytes per pixel.
    Rgba32,
}

impl PixelFormat {
    /// Bytes per pixel for packed formats, or 0 for planar formats.
    #[must_use]
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Yuv420 => 0,
            Self::Rgb24 => 3,
            Self::Rgba32 => 4,
        }
    }
}

/// Colour space of a video frame. No implicit conversion is performed:
/// the caller passes the space explicitly when converting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColourSpace {
    /// BT.601, the default for standard-definition content.
    #[default]
    Bt601,
    /// BT.709, the default for high-definition content.
    Bt709,
}

/// Colour range of a video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColourRange {
    /// Limited (studio) range: Y in 16..=235, chroma in 16..=240.
    #[default]
    Limited,
    /// Full range: Y and chroma span the whole byte range.
    Full,
}

/// Decoded audio: interleaved f32 samples in the range [-1.0, 1.0].
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u32,
    /// Interleaved samples, channel-major within each frame.
    pub samples: Vec<f32>,
    /// Presentation timestamp in microseconds.
    pub timestamp: u64,
}

impl AudioFrame {
    /// Number of sample frames (one frame holds one sample per channel).
    #[must_use]
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.samples.len() / self.channels as usize
    }
}

/// Decoded video: one byte vector per plane, with a stride per plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel layout of `planes`.
    pub format: PixelFormat,
    /// Plane data, in the order the format defines.
    pub planes: Vec<Vec<u8>>,
    /// Bytes per row for each plane.
    pub strides: Vec<usize>,
    /// Presentation timestamp in microseconds.
    pub timestamp: u64,
    /// Colour space of the frame.
    pub colour_space: ColourSpace,
    /// Colour range of the frame.
    pub colour_range: ColourRange,
}

/// A decoded frame, audio or video.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// Decoded audio.
    Audio(AudioFrame),
    /// Decoded video.
    Video(VideoFrame),
}

impl Frame {
    /// Presentation timestamp in microseconds.
    #[must_use]
    pub fn timestamp(&self) -> u64 {
        match self {
            Self::Audio(frame) => frame.timestamp,
            Self::Video(frame) => frame.timestamp,
        }
    }
}

/// One compressed packet, as produced by a demuxer or consumed by a decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Codec the packet belongs to.
    pub codec: Codec,
    /// Compressed packet bytes.
    pub data: Vec<u8>,
    /// Presentation timestamp in microseconds.
    pub timestamp: u64,
    /// True when the packet is independently decodable.
    pub is_keyframe: bool,
}

/// One metadata entry. Invalid UTF-8 is never replaced lossily: the raw
/// bytes are kept and `is_valid_utf8` is false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataEntry {
    /// Field name, for example `TITLE`.
    pub key: String,
    /// Raw field value.
    pub value: Vec<u8>,
    /// True when `value` is valid UTF-8.
    pub is_valid_utf8: bool,
}

impl MetadataEntry {
    /// Builds an entry, detecting UTF-8 validity from the bytes.
    #[must_use]
    pub fn new(key: &str, value: &[u8]) -> Self {
        Self {
            key: key.to_string(),
            value: value.to_vec(),
            is_valid_utf8: std::str::from_utf8(value).is_ok(),
        }
    }

    /// The value as text, or `None` when it is not valid UTF-8.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }
}

/// Container metadata, read once at open time and cached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// All entries in container order.
    pub entries: Vec<MetadataEntry>,
}

impl Metadata {
    /// Creates empty metadata.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// First entry with the given key, or `None`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&MetadataEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_frame_count_divides_by_channels() {
        let frame = AudioFrame {
            sample_rate: 8000,
            channels: 2,
            samples: vec![0.0; 8],
            timestamp: 0,
        };
        assert_eq!(frame.frame_count(), 4);
    }

    #[test]
    fn metadata_entry_detects_invalid_utf8() {
        let entry = MetadataEntry::new("TITLE", &[0xFF, 0xFE]);
        assert!(!entry.is_valid_utf8);
        assert_eq!(entry.as_str(), None);
        let entry = MetadataEntry::new("TITLE", b"Hello");
        assert!(entry.is_valid_utf8);
        assert_eq!(entry.as_str(), Some("Hello"));
    }

    #[test]
    fn codec_names_are_stable() {
        assert_eq!(Codec::Flac.name(), "flac");
        assert_eq!(Codec::Av1.name(), "av1");
        assert_eq!(Codec::Vp9.name(), "vp9");
    }
}
