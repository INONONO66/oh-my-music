use std::sync::atomic::{AtomicU64, Ordering};

use ringbuf::traits::Consumer;
use ringbuf::HeapCons;
use rubato::{FastFixedIn, PolynomialDegree, Resampler};

use crate::dsp::SmoothedParam;
use crate::frame::{db_to_gain, StereoFrame};
use crate::source::AudioSource;

const INTERNAL_CHANNELS: usize = 2;
const MAX_RESAMPLE_RATIO_RELATIVE: f64 = 1.0;
const RESAMPLE_TARGET_CHUNK_FRAMES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum MicSourceError {
    #[error("mic input must have at least one channel")]
    InvalidInputChannels,
    #[error("mic input rate must be non-zero")]
    InvalidInputRate,
    #[error("mic target rate must be non-zero")]
    InvalidTargetRate,
    #[error("failed to create mic resampler: {0}")]
    Resampler(String),
}

pub struct MicSource {
    consumer: HeapCons<f32>,
    input_channels: usize,
    enabled: bool,
    gain_db: SmoothedParam,
    underruns: AtomicU64,
    resampler: Option<FastFixedIn<f32>>,
    resampler_input: Vec<Vec<f32>>,
    resampler_output: Vec<Vec<f32>>,
    resampled_residual: Vec<StereoFrame>,
    resampled_offset: usize,
    resampled_len: usize,
    input_chunk_frames: usize,
}

impl MicSource {
    pub fn new(
        consumer: HeapCons<f32>,
        input_channels: usize,
        input_rate: u32,
        target_rate: u32,
    ) -> Result<Self, MicSourceError> {
        if input_channels == 0 {
            return Err(MicSourceError::InvalidInputChannels);
        }
        if input_rate == 0 {
            return Err(MicSourceError::InvalidInputRate);
        }
        if target_rate == 0 {
            return Err(MicSourceError::InvalidTargetRate);
        }

        let needs_resampler = input_rate != target_rate;
        let input_chunk_frames = if needs_resampler {
            compute_input_chunk_frames(input_rate, target_rate)
        } else {
            0
        };

        let (resampler, output_capacity) = if needs_resampler {
            let ratio = target_rate as f64 / input_rate as f64;
            let resampler = FastFixedIn::<f32>::new(
                ratio,
                MAX_RESAMPLE_RATIO_RELATIVE,
                PolynomialDegree::Linear,
                input_chunk_frames,
                INTERNAL_CHANNELS,
            )
            .map_err(|error| MicSourceError::Resampler(error.to_string()))?;
            let output_capacity = resampler.output_frames_max();
            (Some(resampler), output_capacity)
        } else {
            (None, 0)
        };

        Ok(Self {
            consumer,
            input_channels,
            enabled: true,
            gain_db: SmoothedParam::new(0.0),
            underruns: AtomicU64::new(0),
            resampler,
            resampler_input: vec![vec![0.0; input_chunk_frames]; INTERNAL_CHANNELS],
            resampler_output: vec![vec![0.0; output_capacity]; INTERNAL_CHANNELS],
            resampled_residual: vec![StereoFrame::SILENCE; output_capacity],
            resampled_offset: 0,
            resampled_len: 0,
            input_chunk_frames,
        })
    }

    pub fn underrun_count(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    fn render_without_resampling(&mut self, output: &mut [StereoFrame]) {
        let mut underrun_seen = false;

        for frame in output.iter_mut() {
            if underrun_seen {
                *frame = StereoFrame::SILENCE;
                continue;
            }

            let Some(input) = self.pop_input_frame() else {
                self.register_underrun();
                underrun_seen = true;
                *frame = StereoFrame::SILENCE;
                continue;
            };

            *frame = self.apply_gain(input);
        }
    }

    fn render_with_resampling(&mut self, output: &mut [StereoFrame]) {
        let mut output_offset = 0;

        while output_offset < output.len() {
            if self.resampled_offset >= self.resampled_len && !self.refill_resampled_residual() {
                output[output_offset..].fill(StereoFrame::SILENCE);
                return;
            }

            let available = self.resampled_len.saturating_sub(self.resampled_offset);
            let frames_to_copy = available.min(output.len() - output_offset);

            for frame_index in 0..frames_to_copy {
                let source = self.resampled_residual[self.resampled_offset + frame_index];
                output[output_offset + frame_index] = self.apply_gain(source);
            }

            self.resampled_offset += frames_to_copy;
            output_offset += frames_to_copy;

            if self.resampled_offset >= self.resampled_len {
                self.clear_resampled_residual();
            }
        }
    }

    fn refill_resampled_residual(&mut self) -> bool {
        if !self.fill_resampler_input() {
            self.register_underrun();
            self.clear_resampled_residual();
            return false;
        }

        let Some(resampler) = self.resampler.as_mut() else {
            return false;
        };

        let produced = match resampler.process_into_buffer(
            &self.resampler_input,
            &mut self.resampler_output,
            None,
        ) {
            Ok((_consumed, produced)) => produced,
            Err(_) => {
                self.register_underrun();
                self.clear_resampled_residual();
                return false;
            }
        };

        let frames = produced.min(self.resampled_residual.len());
        if frames == 0 {
            self.register_underrun();
            self.clear_resampled_residual();
            return false;
        }

        for frame_index in 0..frames {
            self.resampled_residual[frame_index] = StereoFrame::new(
                self.resampler_output[0][frame_index],
                self.resampler_output[1][frame_index],
            );
        }

        self.resampled_offset = 0;
        self.resampled_len = frames;
        true
    }

    fn fill_resampler_input(&mut self) -> bool {
        for frame_index in 0..self.input_chunk_frames {
            let Some(frame) = self.pop_input_frame() else {
                return false;
            };

            self.resampler_input[0][frame_index] = frame.left;
            self.resampler_input[1][frame_index] = frame.right;
        }

        true
    }

    fn pop_input_frame(&mut self) -> Option<StereoFrame> {
        let left = self.consumer.try_pop()?;
        if self.input_channels == 1 {
            return Some(StereoFrame::new(left, left));
        }

        let right = self.consumer.try_pop()?;
        for _ in 2..self.input_channels {
            self.consumer.try_pop()?;
        }

        Some(StereoFrame::new(left, right))
    }

    fn apply_gain(&mut self, frame: StereoFrame) -> StereoFrame {
        let gain = db_to_gain(self.gain_db.next_value());
        StereoFrame::new(frame.left * gain, frame.right * gain)
    }

    fn clear_resampled_residual(&mut self) {
        self.resampled_offset = 0;
        self.resampled_len = 0;
    }

    fn register_underrun(&self) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

impl AudioSource for MicSource {
    fn render(&mut self, output: &mut [StereoFrame]) {
        if output.is_empty() {
            return;
        }

        if !self.enabled {
            output.fill(StereoFrame::SILENCE);
            return;
        }

        if self.resampler.is_some() {
            self.render_with_resampling(output);
        } else {
            self.render_without_resampling(output);
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
        self.gain_db.set_target(gain_db, ramp_frames);
    }
}

fn compute_input_chunk_frames(input_rate: u32, target_rate: u32) -> usize {
    let frames =
        (RESAMPLE_TARGET_CHUNK_FRAMES as u64 * input_rate as u64).div_ceil(target_rate as u64);
    frames.max(1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    use ringbuf::traits::{Producer, Split};
    use ringbuf::{HeapProd, HeapRb};

    const EPSILON: f32 = 0.0001;
    const RATE_CONVERSION_TOLERANCE_FRAMES: usize = 192;

    fn new_source(
        input_channels: usize,
        input_rate: u32,
        target_rate: u32,
        capacity: usize,
    ) -> Result<(HeapProd<f32>, MicSource), MicSourceError> {
        let ring = HeapRb::<f32>::new(capacity);
        let (producer, consumer) = ring.split();
        let source = MicSource::new(consumer, input_channels, input_rate, target_rate)?;

        Ok((producer, source))
    }

    fn new_consumer(capacity: usize) -> HeapCons<f32> {
        let ring = HeapRb::<f32>::new(capacity);
        let (_producer, consumer) = ring.split();
        consumer
    }

    fn push_samples(producer: &mut HeapProd<f32>, samples: &[f32]) {
        for sample in samples {
            assert!(producer.try_push(*sample).is_ok());
        }
    }

    fn push_sine(producer: &mut HeapProd<f32>, frames: usize, sample_rate: u32, freq_hz: f32) {
        for frame_index in 0..frames {
            let phase = TAU * freq_hz * frame_index as f32 / sample_rate as f32;
            assert!(producer.try_push(phase.sin() * 0.8).is_ok());
        }
    }

    fn render(source: &mut MicSource, frames: usize) -> Vec<StereoFrame> {
        let mut output = vec![StereoFrame::SILENCE; frames];
        source.render(&mut output);
        output
    }

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {actual} to be within {EPSILON} of {expected}"
        );
    }

    fn peak(frames: &[StereoFrame]) -> f32 {
        frames.iter().fold(0.0_f32, |current, frame| {
            current.max(frame.left.abs()).max(frame.right.abs())
        })
    }

    fn last_non_silent_frame(frames: &[StereoFrame]) -> Option<usize> {
        frames
            .iter()
            .rposition(|frame| frame.left.abs() > EPSILON || frame.right.abs() > EPSILON)
    }

    #[test]
    fn mono_passthrough() -> Result<(), MicSourceError> {
        let (mut producer, mut source) = new_source(1, 48_000, 48_000, 2048)?;
        push_sine(&mut producer, 1024, 48_000, 1_000.0);

        let output = render(&mut source, 1024);

        assert!(peak(&output) > db_to_gain(-6.0));
        for frame in output {
            assert_near(frame.left, frame.right);
        }
        assert_eq!(source.underrun_count(), 0);
        Ok(())
    }

    #[test]
    fn rate_conversion_22050_to_48000() -> Result<(), MicSourceError> {
        let (mut producer, mut source) = new_source(1, 22_050, 48_000, 32_768)?;
        push_sine(&mut producer, 22_050, 22_050, 440.0);

        let output = render(&mut source, 48_000);
        let audible_frames = last_non_silent_frame(&output).map_or(0, |index| index + 1);
        let drift = output.len().abs_diff(audible_frames);

        assert_eq!(output.len(), 48_000);
        assert!(
            drift <= RATE_CONVERSION_TOLERANCE_FRAMES,
            "audible frames {audible_frames}, drift {drift}"
        );
        assert!(peak(&output) > db_to_gain(-6.0));
        Ok(())
    }

    #[test]
    fn underrun_silence_and_counter() -> Result<(), MicSourceError> {
        let (_producer, mut source) = new_source(1, 48_000, 48_000, 16)?;

        let output = render(&mut source, 3);

        assert_near(output[0].left, 0.0);
        assert_near(output[0].right, 0.0);
        assert_near(output[1].left, 0.0);
        assert_near(output[1].right, 0.0);
        assert_near(output[2].left, 0.0);
        assert_near(output[2].right, 0.0);
        assert_eq!(source.underrun_count(), 1);
        Ok(())
    }

    #[test]
    fn stereo_input_preserved() -> Result<(), MicSourceError> {
        let (mut producer, mut source) = new_source(2, 48_000, 48_000, 16)?;
        push_samples(&mut producer, &[0.1, 0.2, 0.3, 0.4]);

        let output = render(&mut source, 2);

        assert_near(output[0].left, 0.1);
        assert_near(output[0].right, 0.2);
        assert_near(output[1].left, 0.3);
        assert_near(output[1].right, 0.4);
        assert!(output.iter().any(|frame| frame.left != frame.right));
        assert_eq!(source.underrun_count(), 0);
        Ok(())
    }

    #[test]
    fn disabled_source_renders_silence() -> Result<(), MicSourceError> {
        let (mut producer, mut source) = new_source(1, 48_000, 48_000, 16)?;
        push_samples(&mut producer, &[0.5, 0.5, 0.5]);
        source.set_enabled(false);

        let output = render(&mut source, 3);

        for frame in output {
            assert_near(frame.left, 0.0);
            assert_near(frame.right, 0.0);
        }
        assert_eq!(source.underrun_count(), 0);
        Ok(())
    }

    #[test]
    fn more_than_two_input_channels_drains_extras() -> Result<(), MicSourceError> {
        let (mut producer, mut source) = new_source(4, 48_000, 48_000, 16)?;
        push_samples(&mut producer, &[0.1, 0.2, 0.9, 0.8, 0.3, 0.4, 0.7, 0.6]);

        let output = render(&mut source, 2);

        assert_near(output[0].left, 0.1);
        assert_near(output[0].right, 0.2);
        assert_near(output[1].left, 0.3);
        assert_near(output[1].right, 0.4);
        assert_eq!(source.underrun_count(), 0);
        Ok(())
    }

    #[test]
    fn invalid_constructor_inputs_return_typed_errors() {
        assert!(matches!(
            MicSource::new(new_consumer(1), 0, 48_000, 48_000),
            Err(MicSourceError::InvalidInputChannels)
        ));
        assert!(matches!(
            MicSource::new(new_consumer(1), 1, 0, 48_000),
            Err(MicSourceError::InvalidInputRate)
        ));
        assert!(matches!(
            MicSource::new(new_consumer(1), 1, 48_000, 0),
            Err(MicSourceError::InvalidTargetRate)
        ));
    }
}
