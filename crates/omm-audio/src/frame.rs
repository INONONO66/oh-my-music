/// A stereo audio frame with left and right channels.
#[derive(Debug, Clone, Copy, Default)]
pub struct StereoFrame {
    pub left: f32,
    pub right: f32,
}

impl StereoFrame {
    /// Silence: both channels at 0.0
    pub const SILENCE: Self = StereoFrame {
        left: 0.0,
        right: 0.0,
    };

    /// Create a new stereo frame with given left and right values.
    pub fn new(left: f32, right: f32) -> Self {
        StereoFrame { left, right }
    }
}

/// Convert decibels to linear gain.
///
/// Formula: `10^(db/20)`
///
/// Special cases:
/// - db < -60.0 or db == -inf: returns 0.0
/// - Otherwise: returns 10^(db/20)
pub fn db_to_gain(db: f32) -> f32 {
    if db < -60.0 || db == f32::NEG_INFINITY {
        0.0
    } else {
        10_f32.powf(db / 20.0)
    }
}

/// Convert linear gain to decibels.
///
/// Formula: `20 * log10(gain)`
///
/// Special cases:
/// - gain ≤ 0.0: returns -inf
/// - Otherwise: returns 20 * log10(gain)
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * gain.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stereo_frame_silence() {
        let silence = StereoFrame::SILENCE;
        assert_eq!(silence.left, 0.0);
        assert_eq!(silence.right, 0.0);
    }

    #[test]
    fn test_stereo_frame_new() {
        let frame = StereoFrame::new(0.5, -0.5);
        assert_eq!(frame.left, 0.5);
        assert_eq!(frame.right, -0.5);
    }

    #[test]
    fn test_db_to_gain_zero_db() {
        let gain = db_to_gain(0.0);
        assert!(
            (gain - 1.0).abs() < 0.001,
            "0 dB should be ~1.0, got {}",
            gain
        );
    }

    #[test]
    fn test_db_to_gain_minus_six_db() {
        let gain = db_to_gain(-6.0);
        assert!(
            (gain - 0.5012).abs() < 0.001,
            "-6 dB should be ~0.5012, got {}",
            gain
        );
    }

    #[test]
    fn test_db_to_gain_minus_sixty_db() {
        let gain = db_to_gain(-60.0);
        // -60 dB = 10^(-60/20) = 10^(-3) = 0.001
        assert!(
            (gain - 0.001).abs() < 0.0005,
            "-60 dB should be ~0.001, got {}",
            gain
        );
    }

    #[test]
    fn test_db_to_gain_neg_infinity() {
        let gain = db_to_gain(f32::NEG_INFINITY);
        assert_eq!(gain, 0.0, "-inf dB should be 0.0");
    }

    #[test]
    fn test_gain_to_db_unity() {
        let db = gain_to_db(1.0);
        assert!(
            (db - 0.0).abs() < 0.001,
            "gain 1.0 should be ~0 dB, got {}",
            db
        );
    }

    #[test]
    fn test_gain_to_db_zero() {
        let db = gain_to_db(0.0);
        assert!(
            db.is_infinite() && db.is_sign_negative(),
            "gain 0.0 should be -inf, got {}",
            db
        );
    }

    #[test]
    fn test_gain_to_db_negative() {
        let db = gain_to_db(-0.5);
        assert!(
            db.is_infinite() && db.is_sign_negative(),
            "negative gain should be -inf, got {}",
            db
        );
    }

    #[test]
    fn test_roundtrip_conversion() {
        let original_db = -6.0;
        let gain = db_to_gain(original_db);
        let back_to_db = gain_to_db(gain);
        assert!(
            (back_to_db - original_db).abs() < 0.001,
            "roundtrip failed: {} -> {} -> {}",
            original_db,
            gain,
            back_to_db
        );
    }
}
