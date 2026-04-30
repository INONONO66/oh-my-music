use crate::dsp::SmoothedParam;
use crate::frame::{db_to_gain, StereoFrame};

/// Apply per-sample dB-based gain to a stereo block.
///
/// `gain` carries the target in **dB** (`0.0` = unity, `-6.0` ≈ ½, ≤ `-60.0` → 0).
/// Each frame multiplies both channels by `db_to_gain(gain.next_value())`.
/// Empty `frames` returns without advancing `gain`.
pub fn apply_gain_block(frames: &mut [StereoFrame], gain: &mut SmoothedParam) {
    if frames.is_empty() {
        return;
    }
    for frame in frames.iter_mut() {
        let linear = db_to_gain(gain.next_value());
        frame.left *= linear;
        frame.right *= linear;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::SmoothedParam;
    use crate::frame::StereoFrame;

    const EPSILON: f32 = 0.001;

    #[test]
    fn test_apply_gain_zero_db_passthrough() {
        let mut frames = vec![StereoFrame::new(0.5, 0.5); 4];
        let mut gain = SmoothedParam::new(0.0);
        apply_gain_block(&mut frames, &mut gain);
        for f in &frames {
            assert!(
                (f.left - 0.5).abs() < EPSILON,
                "L expected 0.5, got {}",
                f.left
            );
            assert!(
                (f.right - 0.5).abs() < EPSILON,
                "R expected 0.5, got {}",
                f.right
            );
        }
    }

    #[test]
    fn test_apply_gain_minus_six_db_halves_amplitude() {
        let expected = 0.5012_f32;
        let mut frames = vec![StereoFrame::new(1.0, 1.0); 4];
        let mut gain = SmoothedParam::new(-6.0);
        apply_gain_block(&mut frames, &mut gain);
        for f in &frames {
            assert!(
                (f.left - expected).abs() < EPSILON,
                "L expected ~{}, got {}",
                expected,
                f.left
            );
            assert!(
                (f.right - expected).abs() < EPSILON,
                "R expected ~{}, got {}",
                expected,
                f.right
            );
        }
    }

    #[test]
    fn test_apply_gain_minus_sixty_db_near_silence() {
        let near_zero = 0.01_f32;
        let mut frames = vec![StereoFrame::new(1.0, 1.0); 4];
        let mut gain = SmoothedParam::new(-60.0);
        apply_gain_block(&mut frames, &mut gain);
        for f in &frames {
            assert!(f.left.abs() < near_zero, "L expected ~0.0, got {}", f.left);
            assert!(
                f.right.abs() < near_zero,
                "R expected ~0.0, got {}",
                f.right
            );
        }
    }

    #[test]
    fn test_apply_gain_empty_slice_no_panic() {
        let mut frames: Vec<StereoFrame> = vec![];
        let mut gain = SmoothedParam::new(0.0);
        apply_gain_block(&mut frames, &mut gain);
        assert!(frames.is_empty());
    }
}
