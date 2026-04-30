use crate::dsp::SmoothedParam;
use crate::frame::StereoFrame;

/// Apply per-sample equal-power (constant-power) pan to a stereo block.
///
/// `pan` carries position in `[-1.0, +1.0]` (`-1` = hard left, `0` = center,
/// `+1` = hard right). Equal-power law:
///   θ = (pan + 1) · π/4,  L = cos(θ),  R = sin(θ).
/// Empty `frames` returns without advancing `pan`.
pub fn apply_pan_block(frames: &mut [StereoFrame], pan: &mut SmoothedParam) {
    if frames.is_empty() {
        return;
    }
    for frame in frames.iter_mut() {
        let theta = (pan.next_value() + 1.0) * std::f32::consts::FRAC_PI_4;
        let l_gain = theta.cos();
        let r_gain = theta.sin();
        frame.left *= l_gain;
        frame.right *= r_gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::SmoothedParam;
    use crate::frame::StereoFrame;

    const EPSILON: f32 = 0.01;
    const SQRT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn test_apply_pan_center_equal_power() {
        let mut frames = vec![StereoFrame::new(1.0, 1.0); 4];
        let mut pan = SmoothedParam::new(0.0);
        apply_pan_block(&mut frames, &mut pan);
        for f in &frames {
            assert!(
                (f.left - SQRT_HALF).abs() < EPSILON,
                "L expected ~{}, got {}",
                SQRT_HALF,
                f.left
            );
            assert!(
                (f.right - SQRT_HALF).abs() < EPSILON,
                "R expected ~{}, got {}",
                SQRT_HALF,
                f.right
            );
        }
    }

    #[test]
    fn test_apply_pan_hard_left() {
        let mut frames = vec![StereoFrame::new(1.0, 1.0); 4];
        let mut pan = SmoothedParam::new(-1.0);
        apply_pan_block(&mut frames, &mut pan);
        for f in &frames {
            assert!(
                (f.left - 1.0).abs() < EPSILON,
                "L expected ~1.0, got {}",
                f.left
            );
            assert!(f.right.abs() < EPSILON, "R expected ~0.0, got {}", f.right);
        }
    }

    #[test]
    fn test_apply_pan_hard_right() {
        let mut frames = vec![StereoFrame::new(1.0, 1.0); 4];
        let mut pan = SmoothedParam::new(1.0);
        apply_pan_block(&mut frames, &mut pan);
        for f in &frames {
            assert!(f.left.abs() < EPSILON, "L expected ~0.0, got {}", f.left);
            assert!(
                (f.right - 1.0).abs() < EPSILON,
                "R expected ~1.0, got {}",
                f.right
            );
        }
    }

    #[test]
    fn test_apply_pan_empty_slice_no_panic() {
        let mut frames: Vec<StereoFrame> = vec![];
        let mut pan = SmoothedParam::new(0.0);
        apply_pan_block(&mut frames, &mut pan);
        assert!(frames.is_empty());
    }
}
