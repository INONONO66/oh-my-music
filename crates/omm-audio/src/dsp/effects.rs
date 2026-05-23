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

struct DelayLine {
    buffer: Vec<f32>,
    index: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            index: 0,
        }
    }

    fn read(&self) -> f32 {
        self.buffer[self.index]
    }

    fn write_and_advance(&mut self, value: f32) {
        self.buffer[self.index] = value;
        self.index += 1;
        if self.index >= self.buffer.len() {
            self.index = 0;
        }
    }
}

struct CombFilter {
    delay: DelayLine,
    feedback: f32,
    damp_state: f32,
    damp_alpha: f32,
}

impl CombFilter {
    fn new(delay_samples: usize, feedback: f32, damp_alpha: f32) -> Self {
        Self {
            delay: DelayLine::new(delay_samples),
            feedback,
            damp_state: 0.0,
            damp_alpha,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.delay.read();
        self.damp_state += self.damp_alpha * (delayed - self.damp_state);
        self.delay
            .write_and_advance(input + self.damp_state * self.feedback);
        delayed
    }
}

struct AllpassFilter {
    delay: DelayLine,
    coeff: f32,
}

impl AllpassFilter {
    fn new(delay_samples: usize, coeff: f32) -> Self {
        Self {
            delay: DelayLine::new(delay_samples),
            coeff,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.delay.read();
        let y = delayed - self.coeff * input;
        self.delay.write_and_advance(input + self.coeff * y);
        y
    }
}

const DAMP_ALPHA: f32 = 0.5440619;
const COMB_SCALE: f32 = 0.25;
const RETURN_TRIM: f32 = 0.35;

const LEFT_PREDELAY: usize = 467;
const RIGHT_PREDELAY: usize = 523;
const LEFT_COMB_DELAYS: [(usize, f32); 4] = [
    (1499, 0.7500),
    (1601, 0.7355),
    (1747, 0.7152),
    (1871, 0.6984),
];
const RIGHT_COMB_DELAYS: [(usize, f32); 4] = [
    (1559, 0.7415),
    (1663, 0.7268),
    (1789, 0.7094),
    (1999, 0.6814),
];
const LEFT_AP_DELAYS: [(usize, f32); 2] = [(431, 0.5), (563, 0.5)];
const RIGHT_AP_DELAYS: [(usize, f32); 2] = [(443, 0.5), (593, 0.5)];

pub struct SimpleReverb {
    send_db: SmoothedParam,
    left_predelay: DelayLine,
    right_predelay: DelayLine,
    left_combs: [CombFilter; 4],
    right_combs: [CombFilter; 4],
    left_allpasses: [AllpassFilter; 2],
    right_allpasses: [AllpassFilter; 2],
}

impl SimpleReverb {
    pub fn new(_sample_rate: u32) -> Self {
        Self {
            send_db: SmoothedParam::new(-60.0),
            left_predelay: DelayLine::new(LEFT_PREDELAY),
            right_predelay: DelayLine::new(RIGHT_PREDELAY),
            left_combs: LEFT_COMB_DELAYS.map(|(len, fb)| CombFilter::new(len, fb, DAMP_ALPHA)),
            right_combs: RIGHT_COMB_DELAYS.map(|(len, fb)| CombFilter::new(len, fb, DAMP_ALPHA)),
            left_allpasses: LEFT_AP_DELAYS.map(|(len, c)| AllpassFilter::new(len, c)),
            right_allpasses: RIGHT_AP_DELAYS.map(|(len, c)| AllpassFilter::new(len, c)),
        }
    }

    pub fn set_send_db(&mut self, send_db: f32, ramp_frames: u32) {
        self.send_db.set_target(send_db, ramp_frames);
    }

    pub fn send_db(&self) -> f32 {
        self.send_db.current()
    }

    pub fn process(&mut self, frames: &mut [StereoFrame]) {
        for frame in frames {
            let send_db = self.send_db.next_value();
            let send_gain = if send_db <= -60.0 {
                0.0
            } else {
                db_to_gain(send_db * 0.5)
            };

            let left_in = self.left_predelay.read();
            self.left_predelay.write_and_advance(frame.left * send_gain);
            let right_in = self.right_predelay.read();
            self.right_predelay
                .write_and_advance(frame.right * send_gain);

            let mut left_sum = 0.0_f32;
            for comb in &mut self.left_combs {
                left_sum += comb.process(left_in);
            }
            left_sum *= COMB_SCALE;

            let mut right_sum = 0.0_f32;
            for comb in &mut self.right_combs {
                right_sum += comb.process(right_in);
            }
            right_sum *= COMB_SCALE;

            for ap in &mut self.left_allpasses {
                left_sum = ap.process(left_sum);
            }
            for ap in &mut self.right_allpasses {
                right_sum = ap.process(right_sum);
            }

            frame.left += left_sum * RETURN_TRIM * send_gain;
            frame.right += right_sum * RETURN_TRIM * send_gain;
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

        let mut frames = vec![StereoFrame::SILENCE; 4096];
        frames[0] = StereoFrame::new(1.0, 1.0);
        reverb.process(&mut frames);

        let tail_energy: f32 = frames[LEFT_PREDELAY + 100..]
            .iter()
            .map(|f| f.left * f.left + f.right * f.right)
            .sum();
        assert!(
            tail_energy > 0.01,
            "reverb should produce audible tail after predelay + comb, got energy {tail_energy}"
        );
    }

    #[test]
    fn reverb_send_does_not_capture_audio_while_muted() {
        let mut reverb = SimpleReverb::new(48_000);
        let mut muted_frames = vec![StereoFrame::new(1.0, 1.0); 4096];
        reverb.process(&mut muted_frames);

        reverb.set_send_db(0.0, 0);
        let mut tail = vec![StereoFrame::SILENCE; 4096];
        reverb.process(&mut tail);

        let tail_energy: f32 = tail
            .iter()
            .map(|f| f.left * f.left + f.right * f.right)
            .sum();
        assert!(
            tail_energy < 0.001,
            "muted send should not preload stale reverb energy, got {tail_energy}"
        );
    }

    #[test]
    fn reverb_stereo_spread_differs_between_channels() {
        let mut reverb = SimpleReverb::new(48_000);
        reverb.set_send_db(0.0, 0);

        let mut frames = vec![StereoFrame::SILENCE; 4096];
        frames[0] = StereoFrame::new(1.0, 1.0);
        reverb.process(&mut frames);

        let mut left_differs = false;
        for frame in &frames[LEFT_PREDELAY..] {
            if (frame.left - frame.right).abs() > 0.001 {
                left_differs = true;
                break;
            }
        }
        assert!(
            left_differs,
            "L/R delay lines differ so identical input should produce stereo spread"
        );
    }
}
