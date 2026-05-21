use std::f32::consts::TAU;

use crate::dsp::SmoothedParam;
use crate::frame::{db_to_gain, StereoFrame};
use crate::source::AudioSource;

const NOTES_HZ: [f32; 8] = [
    261.63, 329.63, 392.00, 523.25, 440.00, 392.00, 329.63, 293.66,
];
const BASS_HZ: [f32; 4] = [130.81, 196.00, 220.00, 146.83];
const STEP_MS: u32 = 220;
const NOTE_GAIN: f32 = 0.32;
const BASS_GAIN: f32 = 0.16;

pub struct MusicalTestSource {
    sample_rate: u32,
    frame_index: u64,
    lead_phase: f32,
    bass_phase: f32,
    enabled: bool,
    gain: SmoothedParam,
}

impl MusicalTestSource {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            frame_index: 0,
            lead_phase: 0.0,
            bass_phase: 0.0,
            enabled: true,
            gain: SmoothedParam::new(1.0),
        }
    }

    fn step_frames(&self) -> u64 {
        u64::from(self.sample_rate) * u64::from(STEP_MS) / 1_000
    }

    fn envelope(&self, frame_in_step: u64, step_frames: u64) -> f32 {
        let attack_frames = (u64::from(self.sample_rate) / 200).max(1);
        let release_frames = (u64::from(self.sample_rate) / 18).max(1);

        if frame_in_step < attack_frames {
            frame_in_step as f32 / attack_frames as f32
        } else {
            let remaining = step_frames.saturating_sub(frame_in_step);
            if remaining < release_frames {
                remaining as f32 / release_frames as f32
            } else {
                1.0
            }
        }
    }

    fn advance_phase(phase: &mut f32, freq_hz: f32, sample_rate: u32) {
        *phase += TAU * freq_hz / sample_rate as f32;
        if *phase >= TAU {
            *phase -= TAU;
        }
    }
}

impl AudioSource for MusicalTestSource {
    fn render(&mut self, output: &mut [StereoFrame]) {
        if !self.enabled {
            output.fill(StereoFrame::SILENCE);
            return;
        }

        let step_frames = self.step_frames().max(1);
        for frame in output.iter_mut() {
            let step = self.frame_index / step_frames;
            let frame_in_step = self.frame_index % step_frames;
            let note = NOTES_HZ[step as usize % NOTES_HZ.len()];
            let bass_freq_hz = BASS_HZ[(step / 2) as usize % BASS_HZ.len()];
            let envelope = self.envelope(frame_in_step, step_frames);

            let lead = (self.lead_phase.sin() + (self.lead_phase * 2.0).sin() * 0.35) * NOTE_GAIN;
            let bass_sample = self.bass_phase.sin() * BASS_GAIN;
            let sample = (lead + bass_sample) * envelope * self.gain.next_value();

            *frame = StereoFrame::new(sample, sample);

            Self::advance_phase(&mut self.lead_phase, note, self.sample_rate);
            Self::advance_phase(&mut self.bass_phase, bass_freq_hz, self.sample_rate);
            self.frame_index = self.frame_index.wrapping_add(1);
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
        self.gain.set_target(db_to_gain(gain_db), ramp_frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;

    fn render_buffer(source: &mut MusicalTestSource, frames: usize) -> Vec<StereoFrame> {
        let mut buf = vec![StereoFrame::SILENCE; frames];
        source.render(&mut buf);
        buf
    }

    fn peak(buf: &[StereoFrame]) -> f32 {
        buf.iter().fold(0.0_f32, |acc, frame| {
            acc.max(frame.left.abs()).max(frame.right.abs())
        })
    }

    #[test]
    fn renders_a_safe_audible_pattern() {
        let mut source = MusicalTestSource::new(SAMPLE_RATE);
        let buf = render_buffer(&mut source, SAMPLE_RATE as usize / 2);

        let peak = peak(&buf);
        assert!(peak > 0.1, "expected audible peak, got {peak}");
        assert!(peak <= 0.75, "expected conservative peak, got {peak}");
    }

    #[test]
    fn changes_note_over_time() {
        let mut source = MusicalTestSource::new(SAMPLE_RATE);
        let first = source.lead_phase;
        let frames = source.step_frames() as usize + 1;
        let _ = render_buffer(&mut source, frames);

        assert_ne!(source.lead_phase, first);
        assert!(source.frame_index > source.step_frames());
    }

    #[test]
    fn disabled_outputs_silence() {
        let mut source = MusicalTestSource::new(SAMPLE_RATE);
        source.set_enabled(false);
        let buf = render_buffer(&mut source, 512);

        for frame in buf {
            assert_eq!(frame.left, 0.0);
            assert_eq!(frame.right, 0.0);
        }
    }

    #[test]
    fn zero_sample_rate_is_safe() {
        let mut source = MusicalTestSource::new(0);
        let buf = render_buffer(&mut source, 128);

        assert!(peak(&buf).is_finite());
    }
}
