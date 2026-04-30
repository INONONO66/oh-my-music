use crate::frame::StereoFrame;

pub fn nan_guard_and_clamp(frames: &mut [StereoFrame]) {
    if frames.is_empty() {
        return;
    }
    for frame in frames.iter_mut() {
        frame.left = sanitize(frame.left);
        frame.right = sanitize(frame.right);
    }
}

fn sanitize(sample: f32) -> f32 {
    if !sample.is_finite() {
        0.0
    } else {
        sample.clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-6;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPSILON
    }

    #[test]
    fn nan_becomes_zero() {
        let mut samples = vec![StereoFrame::new(f32::NAN, f32::NAN)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 0.0));
        assert!(approx(samples[0].right, 0.0));
    }

    #[test]
    fn positive_infinity_becomes_zero() {
        let mut samples = vec![StereoFrame::new(f32::INFINITY, f32::INFINITY)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 0.0));
        assert!(approx(samples[0].right, 0.0));
    }

    #[test]
    fn negative_infinity_becomes_zero() {
        let mut samples = vec![StereoFrame::new(f32::NEG_INFINITY, f32::NEG_INFINITY)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 0.0));
        assert!(approx(samples[0].right, 0.0));
    }

    #[test]
    fn normal_value_passes_through_unchanged() {
        let mut samples = vec![StereoFrame::new(0.5, -0.5)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 0.5));
        assert!(approx(samples[0].right, -0.5));
    }

    #[test]
    fn value_above_one_clamps_to_one() {
        let mut samples = vec![StereoFrame::new(1.5, 1.5)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 1.0));
        assert!(approx(samples[0].right, 1.0));
    }

    #[test]
    fn value_below_neg_one_clamps_to_neg_one() {
        let mut samples = vec![StereoFrame::new(-1.5, -1.5)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, -1.0));
        assert!(approx(samples[0].right, -1.0));
    }

    #[test]
    fn empty_slice_does_not_panic() {
        let mut empty: Vec<StereoFrame> = Vec::new();
        nan_guard_and_clamp(&mut empty);
    }

    #[test]
    fn mixed_left_nan_right_normal_handled_independently() {
        let mut samples = vec![StereoFrame::new(f32::NAN, 0.3)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 0.0));
        assert!(approx(samples[0].right, 0.3));
    }

    #[test]
    fn boundary_value_one_passes_through() {
        let mut samples = vec![StereoFrame::new(1.0, -1.0)];
        nan_guard_and_clamp(&mut samples);
        assert!(approx(samples[0].left, 1.0));
        assert!(approx(samples[0].right, -1.0));
    }
}
