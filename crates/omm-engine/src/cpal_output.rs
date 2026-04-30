use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use omm_audio::frame::StereoFrame;
use omm_audio::runtime::AudioRuntime;

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
}

impl CpalOutput {
    /// Build and start a CPAL output stream, taking ownership of the AudioRuntime.
    /// Returns the CpalOutput which keeps the stream alive (drop = stop).
    pub fn new(runtime: AudioRuntime) -> Result<Self, CpalOutputError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(CpalOutputError::NoDevice)?;

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

        let mut runtime = runtime;
        let mut frames = Vec::with_capacity(omm_audio::MAX_BLOCK_FRAMES);

        let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            if data.is_empty() {
                return;
            }

            if data.len() % usize::from(TARGET_CHANNELS) != 0 {
                return;
            }

            let n_frames = data.len() / usize::from(TARGET_CHANNELS);

            frames.clear();
            frames.resize(n_frames, StereoFrame::SILENCE);
            runtime.render_block(&mut frames);

            frames_to_interleaved(&frames, data);
        };

        let error_callback = |err| eprintln!("CPAL stream error: {err}");

        let stream = device
            .build_output_stream(&config, data_callback, error_callback, None)
            .map_err(|err| CpalOutputError::BuildStream(err.to_string()))?;

        stream
            .play()
            .map_err(|err| CpalOutputError::StartStream(err.to_string()))?;

        Ok(Self { stream })
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

fn frames_to_interleaved(frames: &[StereoFrame], data: &mut [f32]) {
    for (chunk, frame) in data.chunks_exact_mut(usize::from(TARGET_CHANNELS)).zip(frames.iter()) {
        chunk[0] = frame.left;
        chunk[1] = frame.right;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omm_audio::runtime::AudioRuntimeConfig;

    #[test]
    fn frames_to_interleaved_writes_left_right_pairs() {
        let frames = [StereoFrame::new(0.1, 0.2), StereoFrame::new(0.3, 0.4)];
        let mut data = [0.0; 4];

        frames_to_interleaved(&frames, &mut data);

        assert_eq!(data, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    #[ignore]
    fn cpal_output_new_succeeds_on_default_device() {
        let runtime = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: TARGET_SAMPLE_RATE,
        });

        if let Err(err) = CpalOutput::new(runtime) {
            panic!("expected CPAL output to start on default device: {err}");
        }
    }
}
