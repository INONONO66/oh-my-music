use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use omm_audio::frame::StereoFrame;
use omm_audio::runtime::AudioRuntime;
use omm_audio::{ENGINE_CHANNELS, ENGINE_SAMPLE_RATE, MAX_BLOCK_FRAMES};
use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

const TARGET_CHANNELS: u16 = ENGINE_CHANNELS as u16;
const INPUT_BUFFER_SECONDS: usize = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpalIoConfig {
    pub output_device_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpalInputConfig {
    pub input_device_name: Option<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CpalIoError {
    #[error("No default output device available")]
    NoOutputDevice,
    #[error("No default input device available")]
    NoInputDevice,
    #[error("{direction} audio device not found: {name}")]
    DeviceNotFound {
        direction: &'static str,
        name: String,
    },
    #[error("48kHz stereo f32 output is not supported by device: {0}")]
    UnsupportedOutputConfig(String),
    #[error("Input stream configuration is not supported by device: {0}")]
    UnsupportedInputConfig(String),
    #[error("Audio device permission denied or unavailable: {0}")]
    PermissionDenied(String),
    #[error("Failed to build stream: {0}")]
    BuildStream(String),
    #[error("Failed to start stream: {0}")]
    StartStream(String),
}

pub struct CpalIo {
    _output_stream: Stream,
    _input_stream: Option<Stream>,
    bad_buffer_count: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicDeviceConfig {
    pub channels: u16,
    pub sample_rate: u32,
}

impl CpalIo {
    pub fn new(runtime: AudioRuntime, input_stream: Option<Stream>) -> Result<Self, CpalIoError> {
        Self::with_config(runtime, input_stream, CpalIoConfig::default())
    }

    pub fn with_config(
        runtime: AudioRuntime,
        input_stream: Option<Stream>,
        config: CpalIoConfig,
    ) -> Result<Self, CpalIoError> {
        let host = cpal::default_host();
        let device = select_output_device(&host, config.output_device_name.as_deref())?;

        if !supports_output_config(&device)? {
            return Err(CpalIoError::UnsupportedOutputConfig(
                "device did not report 48kHz stereo f32 output support".to_owned(),
            ));
        }

        let config = StreamConfig {
            channels: TARGET_CHANNELS,
            sample_rate: ENGINE_SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Default,
        };

        let bad_buffer_count = Arc::new(AtomicU64::new(0));
        let bad_buffer_count_callback = Arc::clone(&bad_buffer_count);

        let mut runtime = runtime;
        let mut frames: Vec<StereoFrame> = Vec::with_capacity(MAX_BLOCK_FRAMES);

        let data_callback = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            render_output_callback(data, &mut runtime, &mut frames, &bad_buffer_count_callback);
        };
        let error_callback = |_err| {};

        let output_stream = device
            .build_output_stream(&config, data_callback, error_callback, None)
            .map_err(map_build_stream_error)?;

        output_stream.play().map_err(map_start_stream_error)?;
        if let Some(stream) = &input_stream {
            stream.play().map_err(map_start_stream_error)?;
        }

        Ok(Self {
            _output_stream: output_stream,
            _input_stream: input_stream,
            bad_buffer_count,
        })
    }

    pub fn build_microphone_stream() -> Result<(Stream, HeapCons<f32>, MicDeviceConfig), CpalIoError>
    {
        Self::build_microphone_stream_with_config(CpalInputConfig::default())
    }

    pub fn build_microphone_stream_with_config(
        config: CpalInputConfig,
    ) -> Result<(Stream, HeapCons<f32>, MicDeviceConfig), CpalIoError> {
        let host = cpal::default_host();
        let device = select_input_device(&host, config.input_device_name.as_deref())?;
        let supported_config = device
            .default_input_config()
            .map_err(map_default_input_config_error)?;
        let sample_format = supported_config.sample_format();
        let stream_config: StreamConfig = supported_config.clone().into();

        let mic_config = MicDeviceConfig {
            channels: stream_config.channels,
            sample_rate: stream_config.sample_rate,
        };

        let capacity = input_ring_capacity(mic_config.channels, mic_config.sample_rate);
        let (producer, consumer) = HeapRb::<f32>::new(capacity).split();
        let overflow_count = Arc::new(AtomicU64::new(0));

        let stream = build_input_stream_for_format(
            &device,
            &stream_config,
            sample_format,
            producer,
            overflow_count,
        )?;

        Ok((stream, consumer, mic_config))
    }

    pub fn input_device_names() -> Result<Vec<String>, CpalIoError> {
        let host = cpal::default_host();
        let devices = host
            .input_devices()
            .map_err(|err| CpalIoError::UnsupportedInputConfig(err.to_string()))?;
        collect_device_names(devices, "input")
    }

    pub fn output_device_names() -> Result<Vec<String>, CpalIoError> {
        let host = cpal::default_host();
        let devices = host
            .output_devices()
            .map_err(|err| CpalIoError::UnsupportedOutputConfig(err.to_string()))?;
        collect_device_names(devices, "output")
    }

    pub fn bad_buffer_count(&self) -> u64 {
        self.bad_buffer_count.load(Ordering::Relaxed)
    }
}

impl Drop for CpalIo {
    fn drop(&mut self) {
        let _ = self._output_stream.pause();
        if let Some(stream) = &self._input_stream {
            let _ = stream.pause();
        }
    }
}

fn select_output_device(
    host: &cpal::Host,
    requested_name: Option<&str>,
) -> Result<cpal::Device, CpalIoError> {
    if let Some(name) = requested_name {
        let devices = host
            .output_devices()
            .map_err(|err| CpalIoError::UnsupportedOutputConfig(err.to_string()))?;
        return find_device_by_name(devices, "output", name);
    }

    host.default_output_device()
        .ok_or(CpalIoError::NoOutputDevice)
}

fn select_input_device(
    host: &cpal::Host,
    requested_name: Option<&str>,
) -> Result<cpal::Device, CpalIoError> {
    if let Some(name) = requested_name {
        let devices = host
            .input_devices()
            .map_err(|err| CpalIoError::UnsupportedInputConfig(err.to_string()))?;
        return find_device_by_name(devices, "input", name);
    }

    host.default_input_device()
        .ok_or(CpalIoError::NoInputDevice)
}

fn find_device_by_name<I>(
    devices: I,
    direction: &'static str,
    requested_name: &str,
) -> Result<cpal::Device, CpalIoError>
where
    I: IntoIterator<Item = cpal::Device>,
{
    for device in devices {
        let Ok(name) = device_name(&device) else {
            continue;
        };

        if device_name_matches(&name, requested_name) {
            return Ok(device);
        }
    }

    Err(CpalIoError::DeviceNotFound {
        direction,
        name: requested_name.to_owned(),
    })
}

fn collect_device_names<I>(devices: I, direction: &'static str) -> Result<Vec<String>, CpalIoError>
where
    I: IntoIterator<Item = cpal::Device>,
{
    let mut names = Vec::new();
    for device in devices {
        let name = device_name(&device).map_err(|err| match direction {
            "output" => CpalIoError::UnsupportedOutputConfig(err.to_string()),
            _ => CpalIoError::UnsupportedInputConfig(err.to_string()),
        })?;
        names.push(name);
    }
    Ok(names)
}

#[allow(deprecated)]
fn device_name(device: &cpal::Device) -> Result<String, cpal::DeviceNameError> {
    device.name()
}

fn device_name_matches(actual: &str, requested: &str) -> bool {
    actual == requested || actual.eq_ignore_ascii_case(requested)
}

fn supports_output_config(device: &cpal::Device) -> Result<bool, CpalIoError> {
    let configs = device
        .supported_output_configs()
        .map_err(|err| CpalIoError::UnsupportedOutputConfig(err.to_string()))?;

    Ok(configs.into_iter().any(|config| {
        config.sample_format() == SampleFormat::F32
            && config.channels() == TARGET_CHANNELS
            && config.min_sample_rate() <= ENGINE_SAMPLE_RATE
            && config.max_sample_rate() >= ENGINE_SAMPLE_RATE
    }))
}

fn build_input_stream_for_format(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    producer: HeapProd<f32>,
    overflow_count: Arc<AtomicU64>,
) -> Result<Stream, CpalIoError> {
    match sample_format {
        SampleFormat::I8 => build_input_stream::<i8>(device, config, producer, overflow_count),
        SampleFormat::I16 => build_input_stream::<i16>(device, config, producer, overflow_count),
        SampleFormat::I24 => {
            build_input_stream::<cpal::I24>(device, config, producer, overflow_count)
        }
        SampleFormat::I32 => build_input_stream::<i32>(device, config, producer, overflow_count),
        SampleFormat::I64 => build_input_stream::<i64>(device, config, producer, overflow_count),
        SampleFormat::U8 => build_input_stream::<u8>(device, config, producer, overflow_count),
        SampleFormat::U16 => build_input_stream::<u16>(device, config, producer, overflow_count),
        SampleFormat::U24 => {
            build_input_stream::<cpal::U24>(device, config, producer, overflow_count)
        }
        SampleFormat::U32 => build_input_stream::<u32>(device, config, producer, overflow_count),
        SampleFormat::U64 => build_input_stream::<u64>(device, config, producer, overflow_count),
        SampleFormat::F32 => build_input_stream::<f32>(device, config, producer, overflow_count),
        SampleFormat::F64 => build_input_stream::<f64>(device, config, producer, overflow_count),
        format => Err(CpalIoError::UnsupportedInputConfig(format.to_string())),
    }
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut producer: HeapProd<f32>,
    overflow_count: Arc<AtomicU64>,
) -> Result<Stream, CpalIoError>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let data_callback = move |data: &[T], _: &cpal::InputCallbackInfo| {
        push_input_samples(data, &mut producer, &overflow_count);
    };
    let error_callback = |_err| {};

    device
        .build_input_stream(config, data_callback, error_callback, None)
        .map_err(map_build_stream_error)
}

fn render_output_callback(
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
        data.fill(0.0);
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

fn push_input_samples<T>(data: &[T], producer: &mut HeapProd<f32>, overflow_count: &AtomicU64)
where
    T: Sample,
    f32: FromSample<T>,
{
    for sample in data.iter().copied() {
        if producer.try_push(f32::from_sample(sample)).is_err() {
            overflow_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn input_ring_capacity(channels: u16, sample_rate: u32) -> usize {
    usize::from(channels.max(1))
        .saturating_mul(sample_rate.max(1) as usize)
        .saturating_mul(INPUT_BUFFER_SECONDS)
}

fn map_default_input_config_error(err: cpal::DefaultStreamConfigError) -> CpalIoError {
    match err {
        cpal::DefaultStreamConfigError::DeviceNotAvailable => {
            CpalIoError::PermissionDenied(err.to_string())
        }
        cpal::DefaultStreamConfigError::BackendSpecific { err }
            if is_permission_like(&err.description) =>
        {
            CpalIoError::PermissionDenied(err.description)
        }
        other => CpalIoError::UnsupportedInputConfig(other.to_string()),
    }
}

fn map_build_stream_error(err: cpal::BuildStreamError) -> CpalIoError {
    match err {
        cpal::BuildStreamError::DeviceNotAvailable => {
            CpalIoError::PermissionDenied(err.to_string())
        }
        cpal::BuildStreamError::BackendSpecific { err } if is_permission_like(&err.description) => {
            CpalIoError::PermissionDenied(err.description)
        }
        other => CpalIoError::BuildStream(other.to_string()),
    }
}

fn map_start_stream_error(err: cpal::PlayStreamError) -> CpalIoError {
    CpalIoError::StartStream(err.to_string())
}

fn is_permission_like(description: &str) -> bool {
    let lower = description.to_ascii_lowercase();
    lower.contains("permission")
        || lower.contains("denied")
        || lower.contains("not authorized")
        || lower.contains("unauthorized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    use omm_audio::runtime::AudioRuntimeConfig;
    use omm_audio::source::TestToneSource;
    use omm_protocol::{SourceInstanceId, SourceKind, SourceTimelinePlacement};
    use ringbuf::traits::Consumer;

    #[test]
    fn interleave_conversion() {
        let frames = [StereoFrame::new(0.25, -0.25), StereoFrame::new(0.5, -0.5)];
        let mut data = [0.0; 4];

        frames_to_interleaved(&frames, &mut data);

        assert_eq!(data, [0.25, -0.25, 0.5, -0.5]);
    }

    #[test]
    fn permission_denied_maps() {
        let unavailable = map_build_stream_error(cpal::BuildStreamError::DeviceNotAvailable);
        assert!(matches!(unavailable, CpalIoError::PermissionDenied(_)));

        let backend = map_build_stream_error(cpal::BuildStreamError::BackendSpecific {
            err: cpal::BackendSpecificError {
                description: "microphone permission denied".to_owned(),
            },
        });
        assert!(matches!(backend, CpalIoError::PermissionDenied(_)));

        let unsupported = map_build_stream_error(cpal::BuildStreamError::StreamConfigNotSupported);
        assert!(matches!(unsupported, CpalIoError::BuildStream(_)));
    }

    #[test]
    fn device_configs_default_to_system_devices() {
        assert_eq!(CpalIoConfig::default().output_device_name, None);
        assert_eq!(CpalInputConfig::default().input_device_name, None);
    }

    #[test]
    fn device_name_matching_accepts_exact_or_case_insensitive_match() {
        assert!(device_name_matches(
            "MacBook Pro Microphone",
            "MacBook Pro Microphone"
        ));
        assert!(device_name_matches(
            "MacBook Pro Microphone",
            "macbook pro microphone"
        ));
        assert!(!device_name_matches(
            "MacBook Pro Microphone",
            "iPhone Microphone"
        ));
    }

    #[test]
    #[ignore]
    fn audio_device_names_can_be_listed() -> Result<(), CpalIoError> {
        let _inputs = CpalIo::input_device_names()?;
        let _outputs = CpalIo::output_device_names()?;
        Ok(())
    }

    #[test]
    fn malformed_output_buffer_silences_and_counts() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: ENGINE_SAMPLE_RATE,
        });
        assert!(runtime
            .add_source_instance(
                SourceInstanceId::new("glicol:main"),
                SourceKind::Generated,
                None,
                SourceTimelinePlacement::always_on(),
                Box::new(TestToneSource::new(440.0, ENGINE_SAMPLE_RATE))
            )
            .is_ok());

        let mut frames = Vec::with_capacity(MAX_BLOCK_FRAMES);
        let bad_buffer_count = AtomicU64::new(0);
        let mut data = [1.0_f32; 5];

        render_output_callback(&mut data, &mut runtime, &mut frames, &bad_buffer_count);

        assert_eq!(bad_buffer_count.load(Ordering::Relaxed), 1);
        assert_eq!(data, [0.0; 5]);
    }

    #[test]
    fn output_callback_chunks_large_buffers() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: ENGINE_SAMPLE_RATE,
        });
        assert!(runtime
            .add_source_instance(
                SourceInstanceId::new("glicol:main"),
                SourceKind::Generated,
                None,
                SourceTimelinePlacement::always_on(),
                Box::new(TestToneSource::new(440.0, ENGINE_SAMPLE_RATE))
            )
            .is_ok());

        let mut frames = Vec::with_capacity(MAX_BLOCK_FRAMES);
        let initial_capacity = frames.capacity();
        let bad_buffer_count = AtomicU64::new(0);
        let mut data = vec![f32::NAN; 2048 * usize::from(TARGET_CHANNELS)];

        render_output_callback(&mut data, &mut runtime, &mut frames, &bad_buffer_count);

        assert_eq!(bad_buffer_count.load(Ordering::Relaxed), 0);
        assert_eq!(frames.capacity(), initial_capacity);
        assert!(data.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn input_push_drops_overflow() {
        let (mut producer, mut consumer) = HeapRb::<f32>::new(2).split();
        let overflow_count = AtomicU64::new(0);

        push_input_samples(&[0.1_f32, 0.2, 0.3], &mut producer, &overflow_count);

        assert_eq!(consumer.try_pop(), Some(0.1));
        assert_eq!(consumer.try_pop(), Some(0.2));
        assert_eq!(consumer.try_pop(), None);
        assert_eq!(overflow_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[ignore]
    fn stream_lifecycle() -> Result<(), CpalIoError> {
        let (runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: ENGINE_SAMPLE_RATE,
        });

        let io = CpalIo::with_config(runtime, None, CpalIoConfig::default())?;

        thread::sleep(Duration::from_secs(1));
        drop(io);
        Ok(())
    }

    #[test]
    #[ignore]
    fn microphone_pushes_samples() -> Result<(), CpalIoError> {
        let (stream, mut consumer, config) =
            CpalIo::build_microphone_stream_with_config(CpalInputConfig::default())?;
        assert!(config.channels > 0);
        assert!(config.sample_rate > 0);
        stream.play().map_err(map_start_stream_error)?;

        thread::sleep(Duration::from_secs(1));

        let mut count = 0;
        while consumer.try_pop().is_some() {
            count += 1;
        }

        assert!(count > 0, "expected microphone stream to push samples");
        Ok(())
    }
}
