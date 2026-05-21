use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use omm_protocol::params::SourceId;
use realfft::{RealFftPlanner, RealToComplex};
use ringbuf::traits::Consumer;
use ringbuf::HeapCons;

use crate::features::types::{BrightnessLabel, ChannelFeatures, EnergyLabel, TextureLabel, Trend};

const FFT_SIZE: usize = 2048;
const HOP_SIZE: usize = 1024;
pub const DEFAULT_FEATURE_WINDOW_MS: u32 = 2000;
const WINDOW_MS: u32 = DEFAULT_FEATURE_WINDOW_MS;
const SILENCE_DB: f32 = -120.0;
const EPSILON: f32 = 1.0e-12;
const ONSET_DB_THRESHOLD: f32 = -45.0;
const ONSET_RISE_DB: f32 = 6.0;
const TREND_DB_THRESHOLD: f32 = 1.0;
const ROLLOFF_RATIO: f32 = 0.85;

pub struct FeatureAnalyzerHandle {
    shared: Arc<Mutex<AnalyzerShared>>,
    should_shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FeatureAnalyzerHandle {
    pub fn new(sample_rate: u32) -> Self {
        let shared = Arc::new(Mutex::new(AnalyzerShared::new(sample_rate)));
        let should_shutdown = Arc::new(AtomicBool::new(false));
        let thread = spawn_analyzer_thread(Arc::clone(&shared), Arc::clone(&should_shutdown));

        Self {
            shared,
            should_shutdown,
            thread,
        }
    }

    pub fn register_channel(&self, source_id: SourceId, consumer: HeapCons<f32>) {
        register_in_shared(&self.shared, source_id, consumer);
    }

    pub fn poll_features(&mut self, source_id: SourceId) -> Option<ChannelFeatures> {
        let Ok(mut shared) = self.shared.lock() else {
            return None;
        };

        shared
            .channels
            .get_mut(&source_id)
            .and_then(|channel| channel.latest.take())
    }

    pub fn poll_all(&mut self) -> Vec<ChannelFeatures> {
        let Ok(mut shared) = self.shared.lock() else {
            return Vec::new();
        };

        shared
            .channels
            .values_mut()
            .filter_map(|channel| channel.latest.take())
            .collect()
    }

    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    pub(crate) fn registry(&self) -> FeatureRegistry {
        FeatureRegistry {
            shared: Arc::clone(&self.shared),
        }
    }

    fn shutdown_inner(&mut self) {
        self.should_shutdown.store(true, Ordering::Release);

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FeatureAnalyzerHandle {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

pub(crate) struct FeatureRegistry {
    shared: Arc<Mutex<AnalyzerShared>>,
}

impl FeatureRegistry {
    pub(crate) fn register_channel(&self, source_id: SourceId, consumer: HeapCons<f32>) {
        register_in_shared(&self.shared, source_id, consumer);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OfflineFeatureConfig {
    pub window_ms: u32,
    pub hop_ms: u32,
}

impl Default for OfflineFeatureConfig {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_FEATURE_WINDOW_MS,
            hop_ms: 500,
        }
    }
}

pub fn compute_offline_channel_features(
    source_id: SourceId,
    samples: &[f32],
    sample_rate: u32,
    config: OfflineFeatureConfig,
) -> Vec<ChannelFeatures> {
    let sample_rate = sample_rate.max(1);
    let window_samples = ms_to_samples(config.window_ms.max(1), sample_rate).max(FFT_SIZE);
    let hop_samples = ms_to_samples(config.hop_ms.max(1), sample_rate).max(1);
    let mut computer = FeatureComputer::new(sample_rate);
    let mut results = Vec::new();

    if samples.is_empty() {
        let silence = vec![0.0; window_samples];
        results.push(computer.compute_with_duration(source_id, 0, 0, &silence));
        return results;
    }

    let mut start = 0usize;
    while start < samples.len() {
        let end = (start + window_samples).min(samples.len());
        let mut window = samples[start..end]
            .iter()
            .map(|sample| finite_or_zero(*sample))
            .collect::<Vec<_>>();
        let real_window_samples = end.saturating_sub(start);
        let real_window_duration_ms = samples_to_duration_ms(real_window_samples, sample_rate);
        if window.len() < window_samples {
            window.resize(window_samples, 0.0);
        }
        results.push(computer.compute_with_duration(
            source_id,
            start as u64,
            real_window_duration_ms,
            &window,
        ));
        if start + hop_samples >= samples.len() {
            break;
        }
        start += hop_samples;
    }

    results
}

fn ms_to_samples(ms: u32, sample_rate: u32) -> usize {
    ((sample_rate as u64 * ms as u64) / 1_000).max(1) as usize
}

fn samples_to_duration_ms(samples: usize, sample_rate: u32) -> u32 {
    if samples == 0 {
        return 0;
    }
    ((samples as u64 * 1_000).div_ceil(sample_rate.max(1) as u64)) as u32
}

pub(crate) fn analysis_ringbuf_capacity(sample_rate: u32) -> usize {
    let window_samples = ((sample_rate as usize) * WINDOW_MS as usize / 1000).max(1);
    window_samples * 2 + HOP_SIZE * 4
}

fn register_in_shared(
    shared: &Arc<Mutex<AnalyzerShared>>,
    source_id: SourceId,
    consumer: HeapCons<f32>,
) {
    let Ok(mut shared) = shared.lock() else {
        return;
    };

    let window_samples = shared.window_samples;
    shared.channels.insert(
        source_id,
        AnalyzerChannel::new(source_id, consumer, window_samples),
    );
}

struct AnalyzerShared {
    sample_rate: u32,
    window_samples: usize,
    channels: HashMap<SourceId, AnalyzerChannel>,
}

impl AnalyzerShared {
    fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let window_samples = ((sample_rate as usize) * WINDOW_MS as usize / 1000).max(1);

        Self {
            sample_rate,
            window_samples,
            channels: HashMap::new(),
        }
    }
}

struct AnalyzerChannel {
    source_id: SourceId,
    consumer: HeapCons<f32>,
    samples: VecDeque<f32>,
    next_window_start_samples: u64,
    latest: Option<ChannelFeatures>,
}

impl AnalyzerChannel {
    fn new(source_id: SourceId, consumer: HeapCons<f32>, window_samples: usize) -> Self {
        Self {
            source_id,
            consumer,
            samples: VecDeque::with_capacity(window_samples + HOP_SIZE),
            next_window_start_samples: 0,
            latest: None,
        }
    }

    fn drain_ringbuf(&mut self) -> bool {
        let mut did_work = false;

        while let Some(sample) = self.consumer.try_pop() {
            self.samples.push_back(finite_or_zero(sample));
            did_work = true;
        }

        did_work
    }

    fn analyze_ready_windows(
        &mut self,
        window_samples: usize,
        computer: &mut FeatureComputer,
    ) -> bool {
        let mut did_work = false;

        while self.samples.len() >= window_samples {
            let window: Vec<f32> = self.samples.iter().take(window_samples).copied().collect();
            self.latest =
                Some(computer.compute(self.source_id, self.next_window_start_samples, &window));

            let consumed = HOP_SIZE.min(self.samples.len());
            self.samples.drain(..consumed);
            self.next_window_start_samples += consumed as u64;
            did_work = true;
        }

        did_work
    }
}

pub(crate) struct FeatureComputer {
    sample_rate: u32,
    fft: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    fft_input: Vec<f32>,
}

impl FeatureComputer {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let hann = (0..FFT_SIZE)
            .map(|index| {
                let phase = std::f32::consts::TAU * index as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();

        Self {
            sample_rate,
            fft,
            hann,
            fft_input: vec![0.0; FFT_SIZE],
        }
    }

    pub(crate) fn compute(
        &mut self,
        source_id: SourceId,
        window_start_samples: u64,
        samples: &[f32],
    ) -> ChannelFeatures {
        self.compute_with_duration(source_id, window_start_samples, WINDOW_MS, samples)
    }

    fn compute_with_duration(
        &mut self,
        source_id: SourceId,
        window_start_samples: u64,
        window_duration_ms: u32,
        samples: &[f32],
    ) -> ChannelFeatures {
        let dynamics = compute_dynamics(samples);
        let spectrum = self.compute_spectrum(samples);
        let zero_crossing_rate = compute_zero_crossing_rate(samples);
        let onset_rate_per_sec = compute_onset_rate(samples, self.sample_rate);

        let peak_db = finite_or_default(dynamics.peak_db, SILENCE_DB);
        let rms_db = finite_or_default(dynamics.rms_db, SILENCE_DB);
        let crest_factor = finite_or_default(dynamics.crest_factor, 0.0);
        let spectral_centroid_hz = finite_or_default(spectrum.centroid_hz, 0.0);
        let spectral_rolloff_hz = finite_or_default(spectrum.rolloff_hz, 0.0);
        let spectral_flatness = finite_or_default(spectrum.flatness, 0.0).clamp(0.0, 1.0);
        let onset_rate_per_sec = finite_or_default(onset_rate_per_sec, 0.0);
        let zero_crossing_rate = finite_or_default(zero_crossing_rate, 0.0);

        ChannelFeatures {
            source_id,
            window_start_ms: window_start_samples.saturating_mul(1000) / self.sample_rate as u64,
            window_duration_ms,
            peak_db,
            rms_db,
            crest_factor,
            trend: dynamics.trend,
            spectral_centroid_hz,
            spectral_rolloff_hz,
            spectral_flatness,
            onset_rate_per_sec,
            zero_crossing_rate,
            energy_label: EnergyLabel::from_rms_db(rms_db),
            brightness_label: BrightnessLabel::from_spectral_centroid_hz(spectral_centroid_hz),
            texture_label: TextureLabel::from_spectral_flatness(spectral_flatness),
        }
    }

    fn compute_spectrum(&mut self, samples: &[f32]) -> SpectrumFeatures {
        if samples.len() < FFT_SIZE {
            return SpectrumFeatures::default();
        }

        let mut spectrum = self.fft.make_output_vec();
        let mut magnitudes = vec![0.0_f32; FFT_SIZE / 2 + 1];
        let mut frame_count = 0usize;

        for offset in (0..=samples.len() - FFT_SIZE).step_by(HOP_SIZE) {
            for index in 0..FFT_SIZE {
                self.fft_input[index] = finite_or_zero(samples[offset + index]) * self.hann[index];
            }

            if self
                .fft
                .process(&mut self.fft_input, &mut spectrum)
                .is_err()
            {
                continue;
            }

            for (bin, value) in spectrum.iter().enumerate() {
                magnitudes[bin] += value.norm_sqr().sqrt();
            }
            frame_count += 1;
        }

        if frame_count == 0 {
            return SpectrumFeatures::default();
        }

        for magnitude in &mut magnitudes {
            *magnitude /= frame_count as f32;
        }

        summarize_spectrum(&magnitudes, self.sample_rate)
    }
}

#[derive(Default)]
struct SpectrumFeatures {
    centroid_hz: f32,
    rolloff_hz: f32,
    flatness: f32,
}

struct DynamicFeatures {
    peak_db: f32,
    rms_db: f32,
    crest_factor: f32,
    trend: Trend,
}

fn spawn_analyzer_thread(
    shared: Arc<Mutex<AnalyzerShared>>,
    should_shutdown: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("omm-feature-analyzer".to_owned())
        .spawn(move || run_analyzer(shared, should_shutdown))
        .ok()
}

fn run_analyzer(shared: Arc<Mutex<AnalyzerShared>>, should_shutdown: Arc<AtomicBool>) {
    let sample_rate = match shared.lock() {
        Ok(shared) => shared.sample_rate,
        Err(_) => return,
    };
    let mut computer = FeatureComputer::new(sample_rate);
    let idle_sleep = Duration::from_millis(1);

    while !should_shutdown.load(Ordering::Acquire) {
        let did_work = {
            let Ok(mut shared) = shared.lock() else {
                return;
            };
            let window_samples = shared.window_samples;
            let mut did_work = false;

            for channel in shared.channels.values_mut() {
                did_work |= channel.drain_ringbuf();
                did_work |= channel.analyze_ready_windows(window_samples, &mut computer);
            }

            did_work
        };

        if !did_work {
            thread::sleep(idle_sleep);
        }
    }
}

fn compute_dynamics(samples: &[f32]) -> DynamicFeatures {
    if samples.is_empty() {
        return DynamicFeatures {
            peak_db: SILENCE_DB,
            rms_db: SILENCE_DB,
            crest_factor: 0.0,
            trend: Trend::Stable,
        };
    }

    let mut peak = 0.0_f32;
    let mut sum_square = 0.0_f32;
    for &sample in samples {
        let sample = finite_or_zero(sample);
        peak = peak.max(sample.abs());
        sum_square += sample * sample;
    }

    let rms = (sum_square / samples.len() as f32).sqrt();
    DynamicFeatures {
        peak_db: amplitude_to_db(peak),
        rms_db: amplitude_to_db(rms),
        crest_factor: if rms > EPSILON { peak / rms } else { 0.0 },
        trend: compute_trend(samples),
    }
}

fn compute_trend(samples: &[f32]) -> Trend {
    if samples.len() < 2 {
        return Trend::Stable;
    }

    let mid = samples.len() / 2;
    let first_db = amplitude_to_db(rms(&samples[..mid]));
    let second_db = amplitude_to_db(rms(&samples[mid..]));
    let delta = second_db - first_db;

    if delta > TREND_DB_THRESHOLD {
        Trend::Rising
    } else if delta < -TREND_DB_THRESHOLD {
        Trend::Falling
    } else {
        Trend::Stable
    }
}

fn summarize_spectrum(magnitudes: &[f32], sample_rate: u32) -> SpectrumFeatures {
    let total: f32 = magnitudes.iter().copied().sum();
    if total <= EPSILON {
        return SpectrumFeatures::default();
    }

    let bin_hz = sample_rate as f32 / FFT_SIZE as f32;
    let weighted_sum: f32 = magnitudes
        .iter()
        .enumerate()
        .map(|(bin, magnitude)| bin as f32 * bin_hz * magnitude)
        .sum();
    let rolloff_target = total * ROLLOFF_RATIO;
    let mut cumulative = 0.0_f32;
    let mut rolloff_hz = 0.0_f32;

    for (bin, magnitude) in magnitudes.iter().enumerate() {
        cumulative += *magnitude;
        if cumulative >= rolloff_target {
            rolloff_hz = bin as f32 * bin_hz;
            break;
        }
    }

    SpectrumFeatures {
        centroid_hz: weighted_sum / total,
        rolloff_hz,
        flatness: compute_flatness(magnitudes),
    }
}

fn compute_flatness(magnitudes: &[f32]) -> f32 {
    if magnitudes.len() <= 1 {
        return 0.0;
    }

    let bins = &magnitudes[1..];
    let arithmetic_mean = bins.iter().copied().sum::<f32>() / bins.len() as f32;
    if arithmetic_mean <= EPSILON {
        return 0.0;
    }

    let log_mean = bins
        .iter()
        .map(|magnitude| magnitude.max(EPSILON).ln())
        .sum::<f32>()
        / bins.len() as f32;

    (log_mean.exp() / arithmetic_mean).clamp(0.0, 1.0)
}

fn compute_zero_crossing_rate(samples: &[f32]) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }

    let crossings = samples
        .windows(2)
        .filter(|pair| {
            finite_or_zero(pair[0]).is_sign_negative() != finite_or_zero(pair[1]).is_sign_negative()
        })
        .count();

    crossings as f32 / (samples.len() - 1) as f32
}

fn compute_onset_rate(samples: &[f32], sample_rate: u32) -> f32 {
    if samples.len() < HOP_SIZE || sample_rate == 0 {
        return 0.0;
    }

    let mut previous_db = SILENCE_DB;
    let mut onset_count = 0usize;

    for chunk in samples.chunks(HOP_SIZE) {
        let current_db = amplitude_to_db(rms(chunk));
        if current_db > ONSET_DB_THRESHOLD && current_db - previous_db > ONSET_RISE_DB {
            onset_count += 1;
        }
        previous_db = current_db;
    }

    onset_count as f32 / (samples.len() as f32 / sample_rate as f32)
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_square = samples
        .iter()
        .map(|sample| {
            let sample = finite_or_zero(*sample);
            sample * sample
        })
        .sum::<f32>();

    (sum_square / samples.len() as f32).sqrt()
}

fn amplitude_to_db(amplitude: f32) -> f32 {
    if amplitude <= EPSILON || !amplitude.is_finite() {
        SILENCE_DB
    } else {
        (20.0 * amplitude.log10()).max(SILENCE_DB)
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn finite_or_default(value: f32, default: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Producer, Split};
    use ringbuf::{HeapProd, HeapRb};
    use std::f32::consts::TAU;
    use std::time::{Duration, Instant};

    const SAMPLE_RATE: u32 = 48_000;
    const WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * WINDOW_MS as usize / 1000;

    fn new_registered_analyzer(source_id: SourceId) -> (HeapProd<f32>, FeatureAnalyzerHandle) {
        let ring = HeapRb::<f32>::new(WINDOW_SAMPLES + HOP_SIZE * 4);
        let (producer, consumer) = ring.split();
        let analyzer = FeatureAnalyzerHandle::new(SAMPLE_RATE);
        analyzer.register_channel(source_id, consumer);

        (producer, analyzer)
    }

    fn push_samples(producer: &mut HeapProd<f32>, samples: impl IntoIterator<Item = f32>) {
        for sample in samples {
            assert!(producer.try_push(sample).is_ok());
        }
    }

    fn wait_for_feature(
        analyzer: &mut FeatureAnalyzerHandle,
        source_id: SourceId,
    ) -> ChannelFeatures {
        let deadline = Instant::now() + Duration::from_millis(500);

        loop {
            if let Some(features) = analyzer.poll_features(source_id) {
                return features;
            }
            assert!(Instant::now() < deadline, "timed out waiting for features");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn all_fields_are_finite(features: &ChannelFeatures) -> bool {
        features.peak_db.is_finite()
            && features.rms_db.is_finite()
            && features.crest_factor.is_finite()
            && features.spectral_centroid_hz.is_finite()
            && features.spectral_rolloff_hz.is_finite()
            && features.spectral_flatness.is_finite()
            && features.onset_rate_per_sec.is_finite()
            && features.zero_crossing_rate.is_finite()
    }

    #[test]
    fn sine_1khz_centroid() {
        let (mut producer, mut analyzer) = new_registered_analyzer(SourceId::Glicol);
        push_samples(
            &mut producer,
            (0..WINDOW_SAMPLES).map(|index| {
                let phase = TAU * 1_000.0 * index as f32 / SAMPLE_RATE as f32;
                phase.sin() * 0.8
            }),
        );

        let features = wait_for_feature(&mut analyzer, SourceId::Glicol);

        assert!(
            (900.0..=1_100.0).contains(&features.spectral_centroid_hz),
            "centroid {}",
            features.spectral_centroid_hz
        );
        assert!(features.spectral_flatness < 0.2);
        assert_eq!(features.brightness_label, BrightnessLabel::Warm);
        analyzer.shutdown();
    }

    #[test]
    fn white_noise_flatness() {
        let (mut producer, mut analyzer) = new_registered_analyzer(SourceId::System);
        let mut state = 0x1234_5678_u32;
        push_samples(
            &mut producer,
            (0..WINDOW_SAMPLES).map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let normalized = state as f32 / u32::MAX as f32;
                (normalized * 2.0 - 1.0) * 0.5
            }),
        );

        let features = wait_for_feature(&mut analyzer, SourceId::System);

        assert!(
            features.spectral_flatness > 0.6,
            "flatness {}",
            features.spectral_flatness
        );
        assert_eq!(features.texture_label, TextureLabel::Noisy);
        analyzer.shutdown();
    }

    #[test]
    fn offline_tail_window_reports_real_duration() {
        let sample_rate = 48_000;
        let samples = vec![0.25; sample_rate as usize * 5 / 2];
        let features = compute_offline_channel_features(
            SourceId::Player,
            &samples,
            sample_rate,
            OfflineFeatureConfig {
                window_ms: 2_000,
                hop_ms: 1_500,
            },
        );

        assert_eq!(features[0].window_start_ms, 0);
        assert_eq!(features[0].window_duration_ms, 2_000);
        assert_eq!(features[1].window_start_ms, 1_500);
        assert_eq!(features[1].window_duration_ms, 1_000);
    }

    #[test]
    fn silence_no_nan() {
        let (mut producer, mut analyzer) = new_registered_analyzer(SourceId::Mic);
        push_samples(&mut producer, std::iter::repeat(0.0).take(WINDOW_SAMPLES));

        let features = wait_for_feature(&mut analyzer, SourceId::Mic);

        assert!(all_fields_are_finite(&features));
        assert_eq!(features.peak_db, SILENCE_DB);
        assert_eq!(features.rms_db, SILENCE_DB);
        assert_eq!(features.spectral_centroid_hz, 0.0);
        assert_eq!(features.spectral_flatness, 0.0);
        assert_eq!(features.onset_rate_per_sec, 0.0);
        assert_eq!(features.energy_label, EnergyLabel::Silent);
        analyzer.shutdown();
    }

    #[test]
    fn click_train_onsets() {
        let (mut producer, mut analyzer) = new_registered_analyzer(SourceId::Player);
        let interval = SAMPLE_RATE as usize / 10;
        push_samples(
            &mut producer,
            (0..WINDOW_SAMPLES).map(|index| if index % interval == 0 { 1.0 } else { 0.0 }),
        );

        let features = wait_for_feature(&mut analyzer, SourceId::Player);

        assert!(
            (8.0..=11.0).contains(&features.onset_rate_per_sec),
            "onset rate {}",
            features.onset_rate_per_sec
        );
        analyzer.shutdown();
    }

    #[test]
    fn shutdown_within_100ms() {
        let (_producer, analyzer) = new_registered_analyzer(SourceId::Glicol);
        let start = Instant::now();

        analyzer.shutdown();

        assert!(start.elapsed() < Duration::from_millis(100));
    }
}
