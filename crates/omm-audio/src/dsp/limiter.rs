use crate::frame::StereoFrame;

pub struct SafetyLimiter {
    ceiling: f32,
}

impl SafetyLimiter {
    pub fn new(ceiling: f32) -> Self {
        Self { ceiling }
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    pub fn process(&self, frames: &mut [StereoFrame]) {
        if frames.is_empty() {
            return;
        }
        let ceiling = self.ceiling;
        let floor = -ceiling;
        for frame in frames.iter_mut() {
            frame.left = frame.left.clamp(floor, ceiling);
            frame.right = frame.right.clamp(floor, ceiling);
        }
    }
}

impl Default for SafetyLimiter {
    fn default() -> Self {
        Self::new(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-6;

    fn frame(v: f32) -> StereoFrame {
        StereoFrame::new(v, v)
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPSILON
    }

    #[test]
    fn passes_through_in_range_input() {
        let limiter = SafetyLimiter::new(1.0);
        let mut samples = vec![frame(0.5)];
        limiter.process(&mut samples);
        assert!(approx(samples[0].left, 0.5));
        assert!(approx(samples[0].right, 0.5));
    }

    #[test]
    fn clamps_positive_overshoot_to_ceiling() {
        let limiter = SafetyLimiter::new(1.0);
        let mut samples = vec![frame(2.0)];
        limiter.process(&mut samples);
        assert!(approx(samples[0].left, 1.0));
        assert!(approx(samples[0].right, 1.0));
    }

    #[test]
    fn clamps_negative_overshoot_to_floor() {
        let limiter = SafetyLimiter::new(1.0);
        let mut samples = vec![frame(-2.0)];
        limiter.process(&mut samples);
        assert!(approx(samples[0].left, -1.0));
        assert!(approx(samples[0].right, -1.0));
    }

    #[test]
    fn clamps_extreme_positive_value() {
        let limiter = SafetyLimiter::new(1.0);
        let mut samples = vec![frame(100.0)];
        limiter.process(&mut samples);
        assert!(approx(samples[0].left, 1.0));
        assert!(approx(samples[0].right, 1.0));
    }

    #[test]
    fn custom_ceiling_clamps_to_custom_value() {
        let limiter = SafetyLimiter::new(0.5);
        let mut samples = vec![frame(1.0)];
        limiter.process(&mut samples);
        assert!(approx(samples[0].left, 0.5));
        assert!(approx(samples[0].right, 0.5));
    }

    #[test]
    fn custom_ceiling_clamps_negative_to_negative_ceiling() {
        let limiter = SafetyLimiter::new(0.5);
        let mut samples = vec![frame(-1.0)];
        limiter.process(&mut samples);
        assert!(approx(samples[0].left, -0.5));
        assert!(approx(samples[0].right, -0.5));
    }

    #[test]
    fn empty_slice_does_not_panic() {
        let limiter = SafetyLimiter::new(1.0);
        let mut empty: Vec<StereoFrame> = Vec::new();
        limiter.process(&mut empty);
    }

    #[test]
    fn default_ceiling_is_one() {
        let limiter = SafetyLimiter::default();
        assert!(approx(limiter.ceiling(), 1.0));
    }
}
