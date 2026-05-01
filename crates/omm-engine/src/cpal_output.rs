use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use omm_audio::frame::StereoFrame;
use omm_audio::runtime::AudioRuntime;
use omm_audio::MAX_BLOCK_FRAMES;

const TARGET_SAMPLE_RATE: u32 = 48_000;
const TARGET_CHANNELS: u16 = 2;

#[derive(Debug, thiserror::Error)]
pub enum CpalOutputError {
    #[error("No default output device available")]
    NoDevice,
    #[error("48kHz stereo f32 not supported by device: {0}")]
    UnsupportedConfig(String),
    #[error("Failed to build stream: {0}")]
    BuildStream(String),
    #[error("Failed to start stream: {0}")]
    StartStream(String),
}

pub struct CpalOutput {
    stream: Stream,
    bad_buffer_count: Arc<AtomicU64>,
}

impl CpalOutput {
    /// Build and start a CPAL output stream, taking ownership of the AudioRuntime.
    /// Returns the CpalOutput which keeps the stream alive (drop = stop).
    pub fn new(runtime: AudioRuntime) -> Result<Self, CpalOutputError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(CpalOutputError::NoDevice)?;

        if !supports_target_config(&device)? {
            return Err(CpalOutputError::UnsupportedConfig(
                "device did not report 48kHz stereo f32 output support".to_owned(),
            ));
        }

        let config = StreamConfig {
            channels: TARGET_CHANNELS,
            sample_rate: TARGET_SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Default,
        };

        let bad_buffer_count = Arc::new(AtomicU64::new(0));
        let bad_buffer_count_callback = Arc::clone(&bad_buffer_count);

        let mut runtime = runtime;
        let mut frames: Vec<StereoFrame> = Vec::with_capacity(MAX_BLOCK_FRAMES);

        let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            render_callback(data, &mut runtime, &mut frames, &bad_buffer_count_callback);
        };

        let error_callback = |err| eprintln!("CPAL stream error: {err}");

        let stream = device
            .build_output_stream(&config, data_callback, error_callback, None)
            .map_err(|err| CpalOutputError::BuildStream(err.to_string()))?;

        stream
            .play()
            .map_err(|err| CpalOutputError::StartStream(err.to_string()))?;

        Ok(Self {
            stream,
            bad_buffer_count,
        })
    }

    /// Number of CPAL callbacks that received a malformed buffer
    /// (empty or not a multiple of the channel count). Filled with silence.
    pub fn bad_buffer_count(&self) -> u64 {
        self.bad_buffer_count.load(Ordering::Relaxed)
    }
}

impl Drop for CpalOutput {
    fn drop(&mut self) {
        let _ = self.stream.pause();
    }
}

fn supports_target_config(device: &cpal::Device) -> Result<bool, CpalOutputError> {
    let configs = device
        .supported_output_configs()
        .map_err(|err| CpalOutputError::UnsupportedConfig(err.to_string()))?;

    Ok(configs.into_iter().any(|config| {
        config.sample_format() == SampleFormat::F32
            && config.channels() == TARGET_CHANNELS
            && config.min_sample_rate() <= TARGET_SAMPLE_RATE
            && config.max_sample_rate() >= TARGET_SAMPLE_RATE
    }))
}

/// `frames` must be preallocated to `MAX_BLOCK_FRAMES` capacity; the runtime's
/// internal scratch buffers cap at the same budget, so larger CPAL buffers are
/// rendered in chunks rather than reallocating. Malformed buffers (empty or
/// not a multiple of the channel count) are zero-filled and counted in
/// `bad_buffer_count` instead of panicking.
fn render_callback(
    data: &mut [f32],
    runtime: &mut AudioRuntime,
    frames: &mut Vec<StereoFrame>,
    bad_buffer_count: &AtomicU64,
) {
    let channels = usize::from(TARGET_CHANNELS);

    if data.is_empty() {
        bad_buffer_count.fetch_add(1, Ordering::Relaxed);
        return;
    }

    if data.len() % channels != 0 {
        bad_buffer_count.fetch_add(1, Ordering::Relaxed);
        for sample in data.iter_mut() {
            *sample = 0.0;
        }
        return;
    }

    let total_frames = data.len() / channels;
    let mut frames_done = 0;
    while frames_done < total_frames {
        let chunk_len = (total_frames - frames_done).min(MAX_BLOCK_FRAMES);

        frames.resize(chunk_len, StereoFrame::SILENCE);
        runtime.render_block(frames);

        let chunk_start = frames_done * channels;
        let chunk_end = chunk_start + chunk_len * channels;
        frames_to_interleaved(frames, &mut data[chunk_start..chunk_end]);

        frames_done += chunk_len;
    }
}

fn frames_to_interleaved(frames: &[StereoFrame], data: &mut [f32]) {
    for (chunk, frame) in data
        .chunks_exact_mut(usize::from(TARGET_CHANNELS))
        .zip(frames.iter())
    {
        chunk[0] = frame.left;
        chunk[1] = frame.right;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omm_audio::runtime::AudioRuntimeConfig;
    use omm_audio::source::TestToneSource;
    use omm_protocol::SourceId;

    #[test]
    fn frames_to_interleaved_writes_left_right_pairs() {
        let frames = [StereoFrame::new(0.1, 0.2), StereoFrame::new(0.3, 0.4)];
        let mut data = [0.0; 4];

        frames_to_interleaved(&frames, &mut data);

        assert_eq!(data, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn chunked_rendering_2048_frames() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: TARGET_SAMPLE_RATE,
        });
        assert!(runtime
            .add_channel(
                SourceId::Glicol,
                Box::new(TestToneSource::new(440.0, TARGET_SAMPLE_RATE))
            )
            .is_ok());

        let mut frames: Vec<StereoFrame> = Vec::with_capacity(MAX_BLOCK_FRAMES);
        let initial_capacity = frames.capacity();
        let bad_buffer_count = AtomicU64::new(0);

        let total_frames = 2048;
        let channels = usize::from(TARGET_CHANNELS);
        let mut data = vec![f32::NAN; total_frames * channels];

        render_callback(&mut data, &mut runtime, &mut frames, &bad_buffer_count);

        assert_eq!(
            bad_buffer_count.load(Ordering::Relaxed),
            0,
            "well-formed buffer must not be flagged as bad"
        );

        let last_chunk_start = (total_frames - MAX_BLOCK_FRAMES) * channels;
        let last_chunk_peak = data[last_chunk_start..]
            .iter()
            .fold(0.0_f32, |acc, sample| acc.max(sample.abs()));
        assert!(
            last_chunk_peak > 0.3,
            "last 512-frame chunk must contain rendered audio (got peak {last_chunk_peak})"
        );

        for sample in &data {
            assert!(
                sample.is_finite(),
                "all samples must be filled and finite (NaN sentinel left untouched)"
            );
        }

        assert_eq!(
            frames.capacity(),
            initial_capacity,
            "frames buffer must not reallocate beyond MAX_BLOCK_FRAMES"
        );
    }

    #[test]
    fn odd_length_buffer_silent_and_counter() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: TARGET_SAMPLE_RATE,
        });
        assert!(runtime
            .add_channel(
                SourceId::Glicol,
                Box::new(TestToneSource::new(440.0, TARGET_SAMPLE_RATE))
            )
            .is_ok());

        let mut frames: Vec<StereoFrame> = Vec::with_capacity(MAX_BLOCK_FRAMES);
        let bad_buffer_count = AtomicU64::new(0);

        let mut data = vec![1.0_f32; 7];
        render_callback(&mut data, &mut runtime, &mut frames, &bad_buffer_count);

        assert_eq!(
            bad_buffer_count.load(Ordering::Relaxed),
            1,
            "odd-length buffer must increment the bad-buffer counter"
        );
        for (idx, sample) in data.iter().enumerate() {
            assert_eq!(
                *sample, 0.0,
                "odd-length buffer must be zero-filled (sample {idx})"
            );
        }
    }

    #[test]
    fn empty_buffer_increments_counter() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: TARGET_SAMPLE_RATE,
        });

        let mut frames: Vec<StereoFrame> = Vec::with_capacity(MAX_BLOCK_FRAMES);
        let bad_buffer_count = AtomicU64::new(0);

        let mut data: Vec<f32> = Vec::new();
        render_callback(&mut data, &mut runtime, &mut frames, &bad_buffer_count);

        assert_eq!(bad_buffer_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn well_formed_buffer_below_max_block_renders_in_single_chunk() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: TARGET_SAMPLE_RATE,
        });
        assert!(runtime
            .add_channel(
                SourceId::Glicol,
                Box::new(TestToneSource::new(440.0, TARGET_SAMPLE_RATE))
            )
            .is_ok());

        let mut frames: Vec<StereoFrame> = Vec::with_capacity(MAX_BLOCK_FRAMES);
        let bad_buffer_count = AtomicU64::new(0);

        let mut data = vec![0.0_f32; 256 * usize::from(TARGET_CHANNELS)];
        render_callback(&mut data, &mut runtime, &mut frames, &bad_buffer_count);

        assert_eq!(bad_buffer_count.load(Ordering::Relaxed), 0);
        let peak = data
            .iter()
            .fold(0.0_f32, |acc, sample| acc.max(sample.abs()));
        assert!(peak > 0.3, "expected rendered audio, got peak {peak}");
    }

    #[test]
    #[ignore]
    fn cpal_output_new_succeeds_on_default_device() {
        let (runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: TARGET_SAMPLE_RATE,
        });

        if let Err(err) = CpalOutput::new(runtime) {
            panic!("expected CPAL output to start on default device: {err}");
        }
    }
}
