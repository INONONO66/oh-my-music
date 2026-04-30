use crate::frame::{gain_to_db, StereoFrame};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterSnapshot {
    pub peak_left: f32,
    pub peak_right: f32,
    pub rms_left_db: f32,
    pub rms_right_db: f32,
}

impl MeterSnapshot {
    pub fn compute(frames: &[StereoFrame]) -> Self {
        if frames.is_empty() {
            return Self::default();
        }

        let mut peak_left = 0.0_f32;
        let mut peak_right = 0.0_f32;
        let mut sum_sq_left = 0.0_f32;
        let mut sum_sq_right = 0.0_f32;

        for frame in frames {
            let left = frame.left;
            let right = frame.right;
            peak_left = peak_left.max(left.abs());
            peak_right = peak_right.max(right.abs());
            sum_sq_left += left * left;
            sum_sq_right += right * right;
        }

        let count = frames.len() as f32;
        let rms_left = (sum_sq_left / count).sqrt();
        let rms_right = (sum_sq_right / count).sqrt();

        Self {
            peak_left,
            peak_right,
            rms_left_db: gain_to_db(rms_left),
            rms_right_db: gain_to_db(rms_right),
        }
    }
}

impl Default for MeterSnapshot {
    fn default() -> Self {
        Self {
            peak_left: 0.0,
            peak_right: 0.0,
            rms_left_db: f32::NEG_INFINITY,
            rms_right_db: f32::NEG_INFINITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 0.1;

    #[test]
    fn meter_silence_reports_zero_peak_and_negative_infinity_rms() {
        let frames = vec![StereoFrame::SILENCE; 16];
        let snapshot = MeterSnapshot::compute(&frames);
        assert_eq!(snapshot.peak_left, 0.0);
        assert_eq!(snapshot.peak_right, 0.0);
        assert!(snapshot.rms_left_db.is_infinite() && snapshot.rms_left_db.is_sign_negative());
        assert!(snapshot.rms_right_db.is_infinite() && snapshot.rms_right_db.is_sign_negative());
    }

    #[test]
    fn meter_constant_half_reports_minus_six_db_rms() {
        let frames = vec![StereoFrame::new(0.5, -0.5); 16];
        let snapshot = MeterSnapshot::compute(&frames);
        assert!((snapshot.peak_left - 0.5).abs() < EPSILON);
        assert!((snapshot.peak_right - 0.5).abs() < EPSILON);
        assert!((snapshot.rms_left_db + 6.02).abs() < EPSILON);
        assert!((snapshot.rms_right_db + 6.02).abs() < EPSILON);
    }

    #[test]
    fn meter_unity_sine_reports_minus_three_db_rms_and_unity_peak() {
        const SAMPLES_PER_CYCLE: usize = 48;

        let frames: Vec<_> = (0..SAMPLES_PER_CYCLE)
            .map(|index| {
                let phase = std::f32::consts::TAU * index as f32 / SAMPLES_PER_CYCLE as f32;
                let sample = phase.sin();
                StereoFrame::new(sample, sample)
            })
            .collect();

        let snapshot = MeterSnapshot::compute(&frames);

        assert!((snapshot.peak_left - 1.0).abs() < EPSILON);
        assert!((snapshot.peak_right - 1.0).abs() < EPSILON);
        assert!((snapshot.rms_left_db + 3.01).abs() < EPSILON);
        assert!((snapshot.rms_right_db + 3.01).abs() < EPSILON);
    }

    #[test]
    fn meter_empty_slice_returns_default() {
        let snapshot = MeterSnapshot::compute(&[]);
        assert_eq!(snapshot, MeterSnapshot::default());
    }
}
