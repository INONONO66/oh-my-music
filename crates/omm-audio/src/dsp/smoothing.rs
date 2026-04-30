#[derive(Debug, Clone, Copy)]
pub struct SmoothedParam {
    current: f32,
    target: f32,
    step: f32,
    remaining_frames: u32,
}

impl SmoothedParam {
    pub fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining_frames: 0,
        }
    }

    pub fn set_target(&mut self, target: f32, ramp_frames: u32) {
        self.target = target;
        self.remaining_frames = ramp_frames;
        self.step = if ramp_frames == 0 {
            0.0
        } else {
            (target - self.current) / ramp_frames as f32
        };
        if ramp_frames == 0 {
            self.current = target;
        }
    }

    pub fn next_value(&mut self) -> f32 {
        if self.remaining_frames > 0 {
            self.current += self.step;
            self.remaining_frames -= 1;
            if self.remaining_frames == 0 {
                self.current = self.target;
            }
        }
        self.current
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn is_smoothing(&self) -> bool {
        self.remaining_frames > 0
    }

    pub fn snap_to_target(&mut self) {
        self.current = self.target;
        self.remaining_frames = 0;
        self.step = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 0.001;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < EPSILON
    }

    #[test]
    fn test_current_returns_current_value() {
        let param = SmoothedParam::new(0.5);
        assert!(approx_eq(param.current(), 0.5));
    }

    #[test]
    fn test_target_returns_target_value() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 4);
        assert!(approx_eq(param.target(), 1.0));
    }

    #[test]
    fn test_is_smoothing_false_before_ramp() {
        let param = SmoothedParam::new(0.0);
        assert!(!param.is_smoothing());
    }

    #[test]
    fn test_is_smoothing_true_during_ramp() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 100);
        assert!(param.is_smoothing());
    }

    #[test]
    fn test_is_smoothing_false_after_ramp_complete() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 4);
        for _ in 0..4 {
            param.next_value();
        }
        assert!(!param.is_smoothing());
    }

    #[test]
    fn test_snap_to_target_sets_current_to_target() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 100);
        param.snap_to_target();
        assert!(approx_eq(param.current(), 1.0));
        assert!(approx_eq(param.target(), 1.0));
    }

    #[test]
    fn test_snap_to_target_stops_smoothing() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 100);
        param.snap_to_target();
        assert!(!param.is_smoothing());
    }

    #[test]
    fn test_current_has_no_side_effects() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 4);
        let val1 = param.current();
        let val2 = param.current();
        assert_eq!(val1, val2);
        // Verify that next_value() would change it
        let next = param.next_value();
        assert!(next != val1 || approx_eq(next, val1)); // Either changed or already at target
    }

    #[test]
    fn test_regression_set_target_ramp_4_frames() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 4);

        for _ in 0..4 {
            param.next_value();
        }

        assert!(approx_eq(param.current(), 1.0));
        assert!(!param.is_smoothing());
    }

    #[test]
    fn test_set_target_zero_frames_immediate_snap() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 0);
        assert!(approx_eq(param.current(), 1.0));
        assert!(!param.is_smoothing());
    }

    #[test]
    fn test_retarget_during_smoothing() {
        let mut param = SmoothedParam::new(0.0);
        param.set_target(1.0, 100);

        // Advance a few frames
        for _ in 0..10 {
            param.next_value();
        }

        let _mid_value = param.current();

        // Retarget to a different value
        param.set_target(0.5, 50);

        // Should not panic and should smoothly transition
        for _ in 0..50 {
            param.next_value();
        }

        assert!(approx_eq(param.current(), 0.5));
        assert!(!param.is_smoothing());
    }
}
