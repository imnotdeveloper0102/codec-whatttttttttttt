//! Frame-size limits derived from codec level/profile declarations,
//! plus encoding-parameter suggestions.
//!
//! AV1 levels come from Annex A of the AV1 Bitstream & Decoding
//! Process Specification (v1.0.0 with Errata 1): `seq_level_idx`
//! is the 5-bit level read in the sequence header OBU (spec
//! section 5.5.1, parsed in `av1_sequence.rs`), and each defined
//! level caps `MaxPicSize`, `MaxHSize` and `MaxVSize`. The
//! `seq_level_idx` values 2, 3, 6, 7, 10 and 11 (levels 2.2, 2.3,
//! 3.2, 3.3, 4.2, 4.3) are undefined in the spec, and values above
//! 19 (level 7.x and later) are draft or undefined, so they are
//! rejected rather than mapped to a neighbouring level.
//!
//! VP9 has no level system at all: the VP9 Bitstream & Decoding
//! Process Specification v0.6 defines only profiles 0-3 (section
//! 6.2.2), which constrain bit depth and chroma subsampling, not
//! frame size. A VP9 frame size is coded as two `f(16)` fields
//! plus one (section 6.2.3), so the syntax alone allows up to
//! 65536 pixels per side; the bound enforced here is the library
//! limit (`MAX_WIDTH` / `MAX_HEIGHT`, 8192), not a spec level
//! limit, and it is the same for every profile.
//!
//! The bitrate/CRF values in this module are suggestions only.
//! They are not in either spec; they come from common encoder
//! ladders and the caller is free to ignore them. They are
//! computed from the frame's pixel count, never from a label such
//! as "720p": the label never decides anything.

use crate::av1_sequence::SequenceHeader;
use crate::error::{Error, Result};
use crate::limits::{MAX_HEIGHT, MAX_WIDTH};
use crate::vp9_bitstream::Vp9FrameHeader;

/// A frame size in pixels. Width and height are validated on
/// construction: both must be non-zero and within the library
/// limit, so a `FrameSize` can never describe an unrepresentable
/// frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameSize {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
}

impl FrameSize {
    /// Builds a frame size, rejecting a zero side or a side above
    /// the library limit.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a zero width or height and
    /// `Overflow` for a side above `MAX_WIDTH` / `MAX_HEIGHT`.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidArgument("frame size has a zero side"));
        }
        if width > MAX_WIDTH {
            return Err(Error::Overflow("frame width exceeds the library limit"));
        }
        if height > MAX_HEIGHT {
            return Err(Error::Overflow("frame height exceeds the library limit"));
        }
        Ok(Self { width, height })
    }

    /// Number of luma samples in the frame.
    #[must_use]
    pub fn pixel_count(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// One defined AV1 level (spec Annex A, section 10.3). Only the
/// size caps are carried here: the display/decode rates and
/// bitrate caps of a level do not change which frame sizes are
/// legal, so they are not needed to validate a frame. Where a
/// display rate is known it is kept so a caller can also check
/// frame rate; `None` means the rate still needs verification
/// against the spec and is not checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Av1Level {
    /// `seq_level_idx` value from the sequence header.
    pub seq_level_idx: u8,
    /// Human-readable level name, for example "5.1".
    pub name: &'static str,
    /// Maximum luma picture size in samples.
    pub max_pic_size: u64,
    /// Maximum frame width in samples.
    pub max_width: u32,
    /// Maximum frame height in samples.
    pub max_height: u32,
    /// Maximum display rate in samples per second, when verified.
    pub max_display_rate: Option<u64>,
}

/// Defined AV1 levels (spec Annex A, section 10.3). The size
/// columns are taken from the spec table; the display rates are
/// the spec table values where verified, and `None` where the
/// rate still needs verification (level 5.2).
const AV1_LEVELS: [Av1Level; 14] = [
    Av1Level {
        seq_level_idx: 0,
        name: "2.0",
        max_pic_size: 147_456,
        max_width: 2048,
        max_height: 1152,
        max_display_rate: Some(4_423_680),
    },
    Av1Level {
        seq_level_idx: 1,
        name: "2.1",
        max_pic_size: 278_784,
        max_width: 2816,
        max_height: 1584,
        max_display_rate: Some(8_363_520),
    },
    Av1Level {
        seq_level_idx: 4,
        name: "3.0",
        max_pic_size: 665_856,
        max_width: 4352,
        max_height: 2448,
        max_display_rate: Some(19_975_680),
    },
    Av1Level {
        seq_level_idx: 5,
        name: "3.1",
        max_pic_size: 1_065_024,
        max_width: 5504,
        max_height: 3096,
        max_display_rate: Some(31_950_720),
    },
    Av1Level {
        seq_level_idx: 8,
        name: "4.0",
        max_pic_size: 2_359_296,
        max_width: 6144,
        max_height: 3456,
        max_display_rate: Some(70_778_880),
    },
    Av1Level {
        seq_level_idx: 9,
        name: "4.1",
        max_pic_size: 2_359_296,
        max_width: 6144,
        max_height: 3456,
        max_display_rate: Some(141_557_760),
    },
    Av1Level {
        seq_level_idx: 12,
        name: "5.0",
        max_pic_size: 8_912_896,
        max_width: 8192,
        max_height: 4352,
        max_display_rate: Some(267_386_880),
    },
    Av1Level {
        seq_level_idx: 13,
        name: "5.1",
        max_pic_size: 8_912_896,
        max_width: 8192,
        max_height: 4352,
        max_display_rate: Some(534_773_760),
    },
    // Level 5.2 display rate: cần xác minh against the spec table
    // before it is used to reject a stream.
    Av1Level {
        seq_level_idx: 14,
        name: "5.2",
        max_pic_size: 8_912_896,
        max_width: 8192,
        max_height: 4352,
        max_display_rate: None,
    },
    Av1Level {
        seq_level_idx: 15,
        name: "5.3",
        max_pic_size: 8_912_896,
        max_width: 8192,
        max_height: 4352,
        max_display_rate: Some(1_069_547_520),
    },
    Av1Level {
        seq_level_idx: 16,
        name: "6.0",
        max_pic_size: 35_651_584,
        max_width: 16384,
        max_height: 8704,
        max_display_rate: Some(1_069_547_520),
    },
    Av1Level {
        seq_level_idx: 17,
        name: "6.1",
        max_pic_size: 35_651_584,
        max_width: 16384,
        max_height: 8704,
        max_display_rate: Some(2_139_095_040),
    },
    Av1Level {
        seq_level_idx: 18,
        name: "6.2",
        max_pic_size: 35_651_584,
        max_width: 16384,
        max_height: 8704,
        max_display_rate: Some(4_278_190_080),
    },
    Av1Level {
        seq_level_idx: 19,
        name: "6.3",
        max_pic_size: 35_651_584,
        max_width: 16384,
        max_height: 8704,
        max_display_rate: Some(4_278_190_080),
    },
];

/// Looks up a defined AV1 level by its `seq_level_idx`.
///
/// # Errors
///
/// Returns `InvalidArgument` for an undefined index (2, 3, 6, 7,
/// 10, 11) or an index above 19, whose levels are draft or
/// undefined in the spec.
pub fn av1_level(seq_level_idx: u8) -> Result<Av1Level> {
    AV1_LEVELS
        .iter()
        .copied()
        .find(|level| level.seq_level_idx == seq_level_idx)
        .ok_or(Error::InvalidArgument(
            "av1 seq_level_idx is undefined in the spec level table",
        ))
}

/// Checks a frame size against one AV1 level: width, height and
/// picture size must all fit, and when a frame rate is given and
/// the level's display rate is verified, the display rate
/// (width * height * fps) must fit too.
///
/// # Errors
///
/// Returns `InvalidArgument` for an undefined level or for a
/// frame that exceeds the level, and `Overflow` for a frame
/// above the library limit.
pub fn validate_av1_frame(
    seq_level_idx: u8,
    width: u32,
    height: u32,
    fps: Option<u32>,
) -> Result<()> {
    let size = FrameSize::new(width, height)?;
    let level = av1_level(seq_level_idx)?;
    if width > level.max_width || height > level.max_height {
        return Err(Error::InvalidArgument(
            "av1 frame dimensions exceed the declared level",
        ));
    }
    if size.pixel_count() > level.max_pic_size {
        return Err(Error::InvalidArgument(
            "av1 frame picture size exceeds the declared level",
        ));
    }
    if let (Some(rate), Some(fps)) = (level.max_display_rate, fps) {
        if fps == 0 {
            return Err(Error::InvalidArgument("av1 frame rate is zero"));
        }
        let display_rate = size.pixel_count().saturating_mul(u64::from(fps));
        if display_rate > rate {
            return Err(Error::InvalidArgument(
                "av1 display rate exceeds the declared level",
            ));
        }
    }
    Ok(())
}

/// Checks a parsed AV1 sequence header against its own declared
/// level (operating point 0), using the header's maximum frame
/// size. This is header-level validation only: the decoder is
/// parser-only and produces no reconstructed frame, so nothing
/// here claims a frame was decoded.
///
/// # Errors
///
/// Propagates `validate_av1_frame` errors.
pub fn validate_av1_sequence_header(header: &SequenceHeader) -> Result<()> {
    validate_av1_frame(
        header.level(),
        header.max_frame_width(),
        header.max_frame_height(),
        None,
    )
}

/// Smallest defined AV1 level whose size caps fit the frame, or
/// `None` when no defined level fits (for example a frame wider
/// than 16384, which is outside every defined level).
#[must_use]
pub fn minimum_av1_level(width: u32, height: u32) -> Option<Av1Level> {
    let Ok(size) = FrameSize::new(width, height) else {
        return None;
    };
    AV1_LEVELS.iter().copied().find(|level| {
        width <= level.max_width
            && height <= level.max_height
            && size.pixel_count() <= level.max_pic_size
    })
}

/// Frame-size limits for one VP9 profile. VP9 has no level
/// table, so every profile shares the same size bound; the
/// profiles differ only in bit depth and chroma subsampling
/// (VP9 spec section 6.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9ProfileLimits {
    /// Bitstream profile (0..=3).
    pub profile: u32,
    /// Bit depths the profile allows.
    pub bit_depths: &'static [u8],
    /// True when the profile allows 4:2:0 chroma subsampling.
    pub allows_420: bool,
    /// True when the profile allows 4:2:2 / 4:4:4 (no horizontal
    /// subsampling).
    pub allows_422_444: bool,
    /// Maximum frame width in pixels (library limit, not a spec
    /// level limit).
    pub max_width: u32,
    /// Maximum frame height in pixels (library limit, not a spec
    /// level limit).
    pub max_height: u32,
}

/// Limits for a VP9 profile (spec section 6.2.2 for the profile
/// semantics; section 6.2.3 for the frame-size syntax). The size
/// bound is the library limit: the spec gives no per-profile or
/// per-level size cap, so a profile cannot reject a frame on
/// size grounds beyond the library limit.
///
/// # Errors
///
/// Returns `InvalidArgument` for a profile above 3.
pub fn vp9_profile_limits(profile: u32) -> Result<Vp9ProfileLimits> {
    let (bit_depths, allows_420, allows_422_444) = match profile {
        0 => (&[8][..], true, false),
        1 => (&[8][..], false, true),
        2 => (&[10, 12][..], true, false),
        3 => (&[10, 12][..], false, true),
        _ => {
            return Err(Error::InvalidArgument("vp9 profile is above 3"));
        }
    };
    Ok(Vp9ProfileLimits {
        profile,
        bit_depths,
        allows_420,
        allows_422_444,
        max_width: MAX_WIDTH,
        max_height: MAX_HEIGHT,
    })
}

/// Checks a VP9 frame size against its profile. Because VP9 has
/// no level table, this only rejects an unknown profile and a
/// frame above the library limit; it never invents a
/// per-profile size cap.
///
/// # Errors
///
/// Returns `InvalidArgument` for a profile above 3 and
/// `Overflow` for a frame above the library limit.
pub fn validate_vp9_frame(profile: u32, width: u32, height: u32) -> Result<()> {
    let limits = vp9_profile_limits(profile)?;
    let _ = FrameSize::new(width, height)?;
    if width > limits.max_width || height > limits.max_height {
        return Err(Error::Overflow(
            "vp9 frame dimensions exceed the library limit",
        ));
    }
    Ok(())
}

/// Checks a parsed VP9 frame header against its profile. This is
/// header-level validation only: the decoder is parser-only and
/// produces no reconstructed frame, so nothing here claims a
/// frame was decoded.
///
/// # Errors
///
/// Returns `InvalidArgument` for a show-existing frame (it has
/// no dimensions) and propagates `validate_vp9_frame` errors.
pub fn validate_vp9_frame_header(header: &Vp9FrameHeader) -> Result<()> {
    if header.show_existing_frame {
        return Err(Error::InvalidArgument(
            "vp9 show-existing frame has no dimensions to validate",
        ));
    }
    validate_vp9_frame(header.profile, header.frame_width, header.frame_height)
}

/// Suggested encoding parameters for one resolution. These are
/// suggestions, not spec values: a caller may use tighter or
/// looser settings and still produce a conformant stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodingSuggestion {
    /// Suggested minimum bitrate in kilobits per second.
    pub bitrate_kbps_min: u32,
    /// Suggested maximum bitrate in kilobits per second.
    pub bitrate_kbps_max: u32,
    /// Suggested constant-quality value (AV1 CRF / VP9 CQ level).
    pub crf: u8,
}

/// One suggestion band, keyed by pixel count so the choice is
/// computed from the frame size, never from a resolution label.
struct SuggestionBand {
    max_pixels: u64,
    av1_min_kbps: u32,
    av1_max_kbps: u32,
    av1_crf: u8,
    vp9_min_kbps: u32,
    vp9_max_kbps: u32,
    vp9_crf: u8,
}

/// Suggestion bands by pixel count, assuming 30 fps; the caller
/// scales by frame rate (see `suggest_*`). The numbers are
/// ladder-style starting points, not spec values.
const SUGGESTION_BANDS: [SuggestionBand; 8] = [
    SuggestionBand {
        max_pixels: 102_400,
        av1_min_kbps: 300,
        av1_max_kbps: 700,
        av1_crf: 32,
        vp9_min_kbps: 400,
        vp9_max_kbps: 900,
        vp9_crf: 33,
    },
    SuggestionBand {
        max_pixels: 230_400,
        av1_min_kbps: 600,
        av1_max_kbps: 1_200,
        av1_crf: 31,
        vp9_min_kbps: 800,
        vp9_max_kbps: 1_500,
        vp9_crf: 32,
    },
    SuggestionBand {
        max_pixels: 409_920,
        av1_min_kbps: 1_000,
        av1_max_kbps: 2_000,
        av1_crf: 31,
        vp9_min_kbps: 1_300,
        vp9_max_kbps: 2_500,
        vp9_crf: 32,
    },
    SuggestionBand {
        max_pixels: 921_600,
        av1_min_kbps: 2_000,
        av1_max_kbps: 4_000,
        av1_crf: 30,
        vp9_min_kbps: 2_500,
        vp9_max_kbps: 5_000,
        vp9_crf: 31,
    },
    SuggestionBand {
        max_pixels: 2_073_600,
        av1_min_kbps: 4_000,
        av1_max_kbps: 8_000,
        av1_crf: 30,
        vp9_min_kbps: 5_000,
        vp9_max_kbps: 10_000,
        vp9_crf: 31,
    },
    SuggestionBand {
        max_pixels: 3_686_400,
        av1_min_kbps: 8_000,
        av1_max_kbps: 14_000,
        av1_crf: 29,
        vp9_min_kbps: 10_000,
        vp9_max_kbps: 18_000,
        vp9_crf: 30,
    },
    SuggestionBand {
        max_pixels: 8_294_400,
        av1_min_kbps: 16_000,
        av1_max_kbps: 30_000,
        av1_crf: 28,
        vp9_min_kbps: 20_000,
        vp9_max_kbps: 38_000,
        vp9_crf: 29,
    },
    SuggestionBand {
        max_pixels: u64::MAX,
        av1_min_kbps: 40_000,
        av1_max_kbps: 80_000,
        av1_crf: 27,
        vp9_min_kbps: 50_000,
        vp9_max_kbps: 100_000,
        vp9_crf: 28,
    },
];

/// Scales a 30 fps bitrate suggestion to `fps` by the frame-rate
/// ratio, saturating rather than overflowing.
fn scale_bitrate_for_fps(kbps: u32, fps: u32) -> u32 {
    if fps == 0 || fps == 30 {
        return kbps;
    }
    let scaled = (u64::from(kbps) * u64::from(fps)) / 30;
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

fn suggest(size: FrameSize, fps: u32, av1: bool) -> Result<EncodingSuggestion> {
    if fps == 0 {
        return Err(Error::InvalidArgument("frame rate is zero"));
    }
    let mut band = &SUGGESTION_BANDS[SUGGESTION_BANDS.len() - 1];
    for candidate in &SUGGESTION_BANDS {
        if size.pixel_count() <= candidate.max_pixels {
            band = candidate;
            break;
        }
    }
    let (min_kbps, max_kbps, crf) = if av1 {
        (band.av1_min_kbps, band.av1_max_kbps, band.av1_crf)
    } else {
        (band.vp9_min_kbps, band.vp9_max_kbps, band.vp9_crf)
    };
    Ok(EncodingSuggestion {
        bitrate_kbps_min: scale_bitrate_for_fps(min_kbps, fps),
        bitrate_kbps_max: scale_bitrate_for_fps(max_kbps, fps),
        crf,
    })
}

/// Suggested AV1 bitrate/CRF for a frame size at `fps`. The
/// values are suggestions, not spec values.
///
/// # Errors
///
/// Returns `InvalidArgument` for a zero frame rate and
/// propagates `FrameSize::new` errors.
pub fn suggest_av1(width: u32, height: u32, fps: u32) -> Result<EncodingSuggestion> {
    suggest(FrameSize::new(width, height)?, fps, true)
}

/// Suggested VP9 bitrate/CQ for a frame size at `fps`. The
/// values are suggestions, not spec values.
///
/// # Errors
///
/// Returns `InvalidArgument` for a zero frame rate and
/// propagates `FrameSize::new` errors.
pub fn suggest_vp9(width: u32, height: u32, fps: u32) -> Result<EncodingSuggestion> {
    suggest(FrameSize::new(width, height)?, fps, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_rejects_zero_side() {
        assert!(matches!(
            FrameSize::new(0, 720),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn undefined_av1_levels_are_rejected() {
        for idx in [2u8, 3, 6, 7, 10, 11, 20, 31] {
            assert!(
                matches!(av1_level(idx), Err(Error::InvalidArgument(_))),
                "idx {idx}"
            );
        }
    }

    #[test]
    fn minimum_level_grows_with_frame_size() {
        assert_eq!(
            minimum_av1_level(426, 240).map(|l| l.seq_level_idx),
            Some(0)
        );
        assert_eq!(
            minimum_av1_level(1920, 1080).map(|l| l.seq_level_idx),
            Some(8)
        );
        assert_eq!(
            minimum_av1_level(3840, 2160).map(|l| l.seq_level_idx),
            Some(12)
        );
        assert_eq!(
            minimum_av1_level(7680, 4320).map(|l| l.seq_level_idx),
            Some(16)
        );
    }

    #[test]
    fn vp9_profiles_share_the_same_size_limit() {
        for profile in 0..=3 {
            let limits = vp9_profile_limits(profile).unwrap();
            assert_eq!(
                (limits.max_width, limits.max_height),
                (MAX_WIDTH, MAX_HEIGHT)
            );
        }
        assert!(matches!(
            vp9_profile_limits(4),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn suggestions_rise_with_resolution() {
        let small = suggest_av1(640, 360, 30).unwrap();
        let large = suggest_av1(3840, 2160, 30).unwrap();
        assert!(large.bitrate_kbps_min > small.bitrate_kbps_max);
        assert!(large.crf <= small.crf);
    }
}
