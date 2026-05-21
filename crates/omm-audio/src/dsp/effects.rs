use crate::dsp::SmoothedParam;
use crate::frame::{db_to_gain, StereoFrame};
use omm_protocol::SourceEqStatus;
use std::f32::consts::TAU;

pub struct ThreeBandEq {
    low_gain_db: SmoothedParam,
    mid_gain_db: SmoothedParam,
    high_gain_db: SmoothedParam,
    low_alpha: f32,
    high_alpha: f32,
    low_left: f32,
    low_right: f32,
    high_left_in: f32,
    high_left_out: f32,
    high_right_in: f32,
    high_right_out: f32,
}

impl ThreeBandEq {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            low_gain_db: SmoothedParam::new(0.0),
            mid_gain_db: SmoothedParam::new(0.0),
            high_gain_db: SmoothedParam::new(0.0),
            low_alpha: lowpass_alpha(250.0, sample_rate),
            high_alpha: highpass_alpha(4_000.0, sample_rate),
            low_left: 0.0,
            low_right: 0.0,
            high_left_in: 0.0,
            high_left_out: 0.0,
            high_right_in: 0.0,
            high_right_out: 0.0,
        }
    }

    pub fn set_gains_db(&mut self, low_db: f32, mid_db: f32, high_db: f32, ramp_frames: u32) {
        self.low_gain_db.set_target(low_db, ramp_frames);
        self.mid_gain_db.set_target(mid_db, ramp_frames);
        self.high_gain_db.set_target(high_db, ramp_frames);
    }

    pub fn set_low_gain_db(&mut self, low_db: f32, ramp_frames: u32) {
        self.low_gain_db.set_target(low_db, ramp_frames);
    }

    pub fn set_mid_gain_db(&mut self, mid_db: f32, ramp_frames: u32) {
        self.mid_gain_db.set_target(mid_db, ramp_frames);
    }

    pub fn set_high_gain_db(&mut self, high_db: f32, ramp_frames: u32) {
        self.high_gain_db.set_target(high_db, ramp_frames);
    }

    pub fn target_status(&self) -> SourceEqStatus {
        SourceEqStatus {
            low_gain_db: self.low_gain_db.target(),
            mid_gain_db: self.mid_gain_db.target(),
            high_gain_db: self.high_gain_db.target(),
        }
    }

    pub fn status(&self) -> SourceEqStatus {
        SourceEqStatus {
            low_gain_db: self.low_gain_db.current(),
            mid_gain_db: self.mid_gain_db.current(),
            high_gain_db: self.high_gain_db.current(),
        }
    }

    pub fn process(&mut self, frames: &mut [StereoFrame]) {
        for frame in frames {
            let dry_left = frame.left;
            let dry_right = frame.right;

            self.low_left += self.low_alpha * (dry_left - self.low_left);
            self.low_right += self.low_alpha * (dry_right - self.low_right);

            let high_left = self.high_alpha * (self.high_left_out + dry_left - self.high_left_in);
            let high_right =
                self.high_alpha * (self.high_right_out + dry_right - self.high_right_in);
            self.high_left_in = dry_left;
            self.high_right_in = dry_right;
            self.high_left_out = high_left;
            self.high_right_out = high_right;

            let low_left = self.low_left;
            let low_right = self.low_right;
            let mid_left = dry_left - low_left - high_left;
            let mid_right = dry_right - low_right - high_right;

            let low_gain = db_to_gain(self.low_gain_db.next_value());
            let mid_gain = db_to_gain(self.mid_gain_db.next_value());
            let high_gain = db_to_gain(self.high_gain_db.next_value());

            frame.left = low_left * low_gain + mid_left * mid_gain + high_left * high_gain;
            frame.right = low_right * low_gain + mid_right * mid_gain + high_right * high_gain;
        }
    }
}

pub struct SimpleReverb {
    send_db: SmoothedParam,
    left_delay: Vec<f32>,
    right_delay: Vec<f32>,
    write_index: usize,
    feedback: f32,
}

impl SimpleReverb {
    pub fn new(sample_rate: u32) -> Self {
        let delay_frames = ((sample_rate as usize) / 750).clamp(1, 512);
        Self {
            send_db: SmoothedParam::new(-60.0),
            left_delay: vec![0.0; delay_frames],
            right_delay: vec![0.0; delay_frames],
            write_index: 0,
            feedback: 0.38,
        }
    }

    pub fn set_send_db(&mut self, send_db: f32, ramp_frames: u32) {
        self.send_db.set_target(send_db, ramp_frames);
    }

    pub fn send_db(&self) -> f32 {
        self.send_db.current()
    }

    pub fn process(&mut self, frames: &mut [StereoFrame]) {
        if self.left_delay.is_empty() {
            return;
        }

        for frame in frames {
            let delayed_left = self.left_delay[self.write_index];
            let delayed_right = self.right_delay[self.write_index];
            let send_db = self.send_db.next_value();
            let send = if send_db <= -60.0 {
                0.0
            } else {
                db_to_gain(send_db)
            };

            self.left_delay[self.write_index] = frame.left * send + delayed_left * self.feedback;
            self.right_delay[self.write_index] = frame.right * send + delayed_right * self.feedback;

            frame.left += delayed_left * send;
            frame.right += delayed_right * send;

            self.write_index += 1;
            if self.write_index >= self.left_delay.len() {
                self.write_index = 0;
            }
        }
    }
}

fn lowpass_alpha(cutoff_hz: f32, sample_rate: u32) -> f32 {
    if sample_rate == 0 || cutoff_hz <= 0.0 {
        return 0.0;
    }
    (1.0 - (-TAU * cutoff_hz / sample_rate as f32).exp()).clamp(0.0, 1.0)
}

fn highpass_alpha(cutoff_hz: f32, sample_rate: u32) -> f32 {
    if sample_rate == 0 || cutoff_hz <= 0.0 {
        return 1.0;
    }
    (-TAU * cutoff_hz / sample_rate as f32)
        .exp()
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_band_eq_reports_and_applies_gain_targets() {
        let mut eq = ThreeBandEq::new(48_000);
        eq.set_gains_db(6.0, -3.0, 4.0, 0);
        assert_eq!(
            eq.status(),
            SourceEqStatus {
                low_gain_db: 6.0,
                mid_gain_db: -3.0,
                high_gain_db: 4.0,
            }
        );

        let mut frames = vec![StereoFrame::new(0.5, 0.5); 128];
        eq.process(&mut frames);
        assert!(frames.iter().any(|frame| frame.left.abs() > 0.01));
    }

    #[test]
    fn simple_reverb_creates_delayed_tail() {
        let mut reverb = SimpleReverb::new(48_000);
        reverb.set_send_db(0.0, 0);
        let mut frames = vec![StereoFrame::SILENCE; 128];
        frames[0] = StereoFrame::new(1.0, 1.0);

        reverb.process(&mut frames);

        assert!(
            frames.iter().skip(1).any(|frame| frame.left.abs() > 0.1),
            "reverb should feed delayed energy back into the source block"
        );
    }

    #[test]
    fn reverb_send_does_not_capture_audio_while_muted() {
        let mut reverb = SimpleReverb::new(48_000);
        let mut muted_frames = vec![StereoFrame::new(1.0, 1.0); 128];
        reverb.process(&mut muted_frames);

        reverb.set_send_db(0.0, 0);
        let mut tail = vec![StereoFrame::SILENCE; 128];
        reverb.process(&mut tail);

        assert!(
            tail.iter().all(|frame| frame.left.abs() < 0.000001),
            "muted send should not preload stale reverb energy"
        );
    }
}
