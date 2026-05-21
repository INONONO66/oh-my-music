use std::path::Path;

use omm_protocol::params::SourceId;
use serde::{Deserialize, Serialize};

use crate::features::analyzer::{compute_offline_channel_features, OfflineFeatureConfig};
use crate::features::types::{BrightnessLabel, ChannelFeatures, EnergyLabel, TextureLabel, Trend};
use crate::frame::StereoFrame;
use crate::source::{AudioSource, PlayerSource, PlayerSourceError};

const DEFAULT_ASSET_ID: &str = "offline-audio";
pub const AUDIO_UNDERSTANDING_SCHEMA_VERSION: u32 = 1;
const MIN_SECTION_MS: u64 = 2_000;
const MIN_BEAT_BPM: f32 = 60.0;
const MAX_BEAT_BPM: f32 = 180.0;
const MAX_EXPLICIT_BEAT_MARKERS: usize = 512;
const DEFAULT_MAX_ANALYSIS_DURATION_MS: u64 = 15 * 60 * 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawFeatureTimeline {
    pub asset_id: String,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub frames: Vec<RawFeatureFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawFeatureFrame {
    pub window_start_ms: u64,
    pub window_duration_ms: u32,
    pub peak_db: f32,
    pub rms_db: f32,
    pub crest_factor: f32,
    pub trend: Trend,
    pub spectral_centroid_hz: f32,
    pub spectral_rolloff_hz: f32,
    pub spectral_flatness: f32,
    pub onset_rate_per_sec: f32,
    pub zero_crossing_rate: f32,
    pub energy_label: EnergyLabel,
    pub brightness_label: BrightnessLabel,
    pub texture_label: TextureLabel,
}

impl From<ChannelFeatures> for RawFeatureFrame {
    fn from(features: ChannelFeatures) -> Self {
        Self {
            window_start_ms: features.window_start_ms,
            window_duration_ms: features.window_duration_ms,
            peak_db: features.peak_db,
            rms_db: features.rms_db,
            crest_factor: features.crest_factor,
            trend: features.trend,
            spectral_centroid_hz: features.spectral_centroid_hz,
            spectral_rolloff_hz: features.spectral_rolloff_hz,
            spectral_flatness: features.spectral_flatness,
            onset_rate_per_sec: features.onset_rate_per_sec,
            zero_crossing_rate: features.zero_crossing_rate,
            energy_label: features.energy_label,
            brightness_label: features.brightness_label,
            texture_label: features.texture_label,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceMap {
    pub entries: Vec<EvidenceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceEntry {
    pub id: String,
    pub feature: EvidenceFeature,
    pub frame_range: FrameRange,
    pub summary: String,
    pub strength: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrameRange {
    pub start_index: usize,
    pub end_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvidenceFeature {
    Dynamics,
    Spectrum,
    Rhythm,
    Texture,
    Trend,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticAudioUnderstanding {
    pub schema_version: u32,
    pub asset_id: String,
    pub duration_ms: u64,
    pub raw_features: RawFeatureTimeline,
    pub evidence: EvidenceMap,
    pub sections: Vec<AudioSection>,
    pub flow: FlowUnderstanding,
    pub tempo: TempoUnderstanding,
    pub moods: Vec<MoodEstimate>,
    pub components: Vec<ComponentEstimate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioSection {
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: SectionLabel,
    pub confidence: f32,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SectionLabel {
    Intro,
    Groove,
    Build,
    Break,
    Plateau,
    Outro,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlowUnderstanding {
    pub summary: String,
    pub confidence: f32,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TempoUnderstanding {
    pub estimated_bpm: Option<f32>,
    pub beat_grid: Vec<BeatMarker>,
    pub beat_grid_summary: Option<BeatGridSummary>,
    pub confidence: f32,
    pub fallback: Option<String>,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BeatGridSummary {
    pub interval_ms: u64,
    pub beat_count: u64,
    pub grid_start_ms: u64,
    pub grid_end_ms: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BeatMarker {
    pub position_ms: u64,
    pub kind: BeatMarkerKind,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BeatMarkerKind {
    Beat,
    Downbeat,
    Bar,
    Phrase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MoodEstimate {
    pub descriptor: MoodDescriptor,
    pub confidence: f32,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MoodDescriptor {
    Bright,
    Warm,
    Dense,
    Sparse,
    Tense,
    Settled,
    Rising,
    Falling,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComponentEstimate {
    pub component: MusicalComponent,
    pub confidence: f32,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MusicalComponent {
    Drums,
    Bass,
    Pad,
    Lead,
    VocalLike,
    NoiseTexture,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioUnderstandingConfig {
    pub sample_rate: u32,
    pub feature_config: OfflineFeatureConfig,
    pub max_analysis_duration_ms: u64,
}

impl Default for AudioUnderstandingConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            feature_config: OfflineFeatureConfig::default(),
            max_analysis_duration_ms: DEFAULT_MAX_ANALYSIS_DURATION_MS,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioUnderstandingError {
    #[error("failed to decode audio file: {0}")]
    Decode(#[from] PlayerSourceError),
    #[error("audio duration {duration_ms} ms exceeds supported max {max_duration_ms} ms")]
    InputTooLong {
        duration_ms: u64,
        max_duration_ms: u64,
    },
}

pub struct OfflineAudioUnderstandingAnalyzer {
    config: AudioUnderstandingConfig,
}

impl OfflineAudioUnderstandingAnalyzer {
    pub fn new(config: AudioUnderstandingConfig) -> Self {
        Self { config }
    }

    pub fn analyze_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<SemanticAudioUnderstanding, AudioUnderstandingError> {
        let mut player = PlayerSource::from_path(path, self.config.sample_rate)?;
        let frame_count = player.duration_frames();
        validate_analysis_frame_count(
            frame_count,
            self.config.sample_rate,
            self.config.max_analysis_duration_ms,
        )?;
        let mut frames = vec![StereoFrame::SILENCE; frame_count];
        player.render(&mut frames);
        Ok(self.analyze_frames(DEFAULT_ASSET_ID, &frames, self.config.sample_rate))
    }

    pub fn analyze_frames(
        &self,
        asset_id: impl Into<String>,
        frames: &[StereoFrame],
        sample_rate: u32,
    ) -> SemanticAudioUnderstanding {
        let mono = downmix(frames);
        self.analyze_mono_samples(asset_id, &mono, sample_rate)
    }

    pub fn analyze_mono_samples(
        &self,
        asset_id: impl Into<String>,
        samples: &[f32],
        sample_rate: u32,
    ) -> SemanticAudioUnderstanding {
        let asset_id = asset_id.into();
        let sample_rate = sample_rate.max(1);
        let duration_ms = samples.len() as u64 * 1_000 / sample_rate as u64;
        let channel_features = compute_offline_channel_features(
            SourceId::Player,
            samples,
            sample_rate,
            self.config.feature_config,
        );
        let raw_frames = channel_features
            .into_iter()
            .map(RawFeatureFrame::from)
            .collect();
        let raw_features = RawFeatureTimeline {
            asset_id: asset_id.clone(),
            sample_rate,
            duration_ms,
            frames: raw_frames,
        };
        let evidence = build_evidence_map(&raw_features.frames);
        let sections = infer_sections(&raw_features.frames, duration_ms, &evidence);
        let tempo = infer_tempo(&raw_features.frames, duration_ms, &evidence);
        let moods = infer_moods(&raw_features.frames, &evidence);
        let components = infer_components(&raw_features.frames, &evidence, tempo.confidence);
        let flow = summarize_flow(&sections, &raw_features.frames, &evidence);

        SemanticAudioUnderstanding {
            schema_version: AUDIO_UNDERSTANDING_SCHEMA_VERSION,
            asset_id,
            duration_ms,
            raw_features,
            evidence,
            sections,
            flow,
            tempo,
            moods,
            components,
        }
    }
}

fn downmix(frames: &[StereoFrame]) -> Vec<f32> {
    frames
        .iter()
        .map(|frame| ((frame.left + frame.right) * 0.5).clamp(-1.0, 1.0))
        .collect()
}

fn validate_analysis_frame_count(
    frame_count: usize,
    sample_rate: u32,
    max_duration_ms: u64,
) -> Result<(), AudioUnderstandingError> {
    let duration_ms = duration_ms_for_frames(frame_count, sample_rate);
    if duration_ms > max_duration_ms {
        return Err(AudioUnderstandingError::InputTooLong {
            duration_ms,
            max_duration_ms,
        });
    }
    Ok(())
}

fn duration_ms_for_frames(frame_count: usize, sample_rate: u32) -> u64 {
    let sample_rate = sample_rate.max(1) as u128;
    let duration_ms = frame_count as u128 * 1_000 / sample_rate;
    duration_ms.min(u64::MAX as u128) as u64
}

fn build_evidence_map(frames: &[RawFeatureFrame]) -> EvidenceMap {
    if frames.is_empty() {
        return EvidenceMap {
            entries: Vec::new(),
        };
    }

    let all = FrameRange {
        start_index: 0,
        end_index: frames.len() - 1,
    };
    let avg_rms = average(frames.iter().map(|frame| frame.rms_db));
    let avg_centroid = average(frames.iter().map(|frame| frame.spectral_centroid_hz));
    let avg_onsets = average(frames.iter().map(|frame| frame.onset_rate_per_sec));
    let avg_flatness = average(frames.iter().map(|frame| frame.spectral_flatness));
    let rising = frames
        .iter()
        .filter(|frame| frame.trend == Trend::Rising)
        .count();
    let falling = frames
        .iter()
        .filter(|frame| frame.trend == Trend::Falling)
        .count();

    EvidenceMap {
        entries: vec![
            EvidenceEntry {
                id: "ev-dynamics".to_string(),
                feature: EvidenceFeature::Dynamics,
                frame_range: all.clone(),
                summary: format!(
                    "average RMS {avg_rms:.1} dB across {} feature windows",
                    frames.len()
                ),
                strength: confidence_from_level(avg_rms, -60.0, -12.0),
            },
            EvidenceEntry {
                id: "ev-spectrum".to_string(),
                feature: EvidenceFeature::Spectrum,
                frame_range: all.clone(),
                summary: format!("average spectral centroid {avg_centroid:.0} Hz"),
                strength: confidence_from_level(avg_centroid, 300.0, 4_000.0),
            },
            EvidenceEntry {
                id: "ev-rhythm".to_string(),
                feature: EvidenceFeature::Rhythm,
                frame_range: all.clone(),
                summary: format!("average onset rate {avg_onsets:.2} onsets/sec"),
                strength: confidence_from_level(avg_onsets, 0.5, 4.0),
            },
            EvidenceEntry {
                id: "ev-texture".to_string(),
                feature: EvidenceFeature::Texture,
                frame_range: all.clone(),
                summary: format!("average spectral flatness {avg_flatness:.2}"),
                strength: avg_flatness.clamp(0.0, 1.0),
            },
            EvidenceEntry {
                id: "ev-trend".to_string(),
                feature: EvidenceFeature::Trend,
                frame_range: all,
                summary: format!("{} rising windows, {} falling windows", rising, falling),
                strength: ((rising.max(falling) as f32) / frames.len() as f32).clamp(0.0, 1.0),
            },
        ],
    }
}

fn infer_sections(
    frames: &[RawFeatureFrame],
    duration_ms: u64,
    evidence: &EvidenceMap,
) -> Vec<AudioSection> {
    if duration_ms == 0 {
        return Vec::new();
    }
    if frames.is_empty() || duration_ms <= MIN_SECTION_MS * 2 {
        return vec![AudioSection {
            start_ms: 0,
            end_ms: duration_ms.max(1),
            label: SectionLabel::Plateau,
            confidence: 0.35,
            evidence_ids: evidence_ids(evidence, &[EvidenceFeature::Dynamics]),
        }];
    }

    let mut changes = Vec::new();
    for (index, pair) in frames.windows(2).enumerate() {
        let energy_delta = (pair[1].rms_db - pair[0].rms_db).abs();
        let onset_delta = (pair[1].onset_rate_per_sec - pair[0].onset_rate_per_sec).abs();
        let centroid_delta = (pair[1].spectral_centroid_hz - pair[0].spectral_centroid_hz).abs();
        if energy_delta >= 3.0 || onset_delta >= 0.75 || centroid_delta >= 1_000.0 {
            changes.push((
                index + 1,
                energy_delta + onset_delta * 2.0 + centroid_delta / 1_000.0,
            ));
        }
    }
    changes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut boundaries = vec![0_u64, duration_ms];
    for (frame_index, _) in changes.into_iter().take(3) {
        let boundary = frames[frame_index].window_start_ms.min(duration_ms);
        if boundary >= MIN_SECTION_MS
            && duration_ms.saturating_sub(boundary) >= MIN_SECTION_MS
            && !boundaries
                .iter()
                .any(|existing| existing.abs_diff(boundary) < MIN_SECTION_MS)
        {
            boundaries.push(boundary);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    boundaries
        .windows(2)
        .enumerate()
        .map(|(index, range)| {
            let start_ms = range[0];
            let end_ms = range[1].max(start_ms + 1);
            let local_frames: Vec<&RawFeatureFrame> = frames
                .iter()
                .filter(|frame| frame.window_start_ms >= start_ms && frame.window_start_ms < end_ms)
                .collect();
            let avg_rms = average(local_frames.iter().map(|frame| frame.rms_db));
            let avg_onsets = average(local_frames.iter().map(|frame| frame.onset_rate_per_sec));
            let avg_trend = dominant_trend(local_frames.iter().map(|frame| frame.trend));
            let label = section_label(index, boundaries.len() - 1, avg_rms, avg_onsets, avg_trend);
            AudioSection {
                start_ms,
                end_ms,
                label,
                confidence: if local_frames.len() >= 2 { 0.62 } else { 0.42 },
                evidence_ids: evidence_ids(
                    evidence,
                    &[
                        EvidenceFeature::Dynamics,
                        EvidenceFeature::Rhythm,
                        EvidenceFeature::Trend,
                    ],
                ),
            }
        })
        .collect()
}

fn section_label(
    index: usize,
    total: usize,
    avg_rms: f32,
    avg_onsets: f32,
    trend: Trend,
) -> SectionLabel {
    if index == 0 && (avg_rms < -28.0 || avg_onsets < 1.0) {
        SectionLabel::Intro
    } else if index + 1 == total && (avg_rms < -28.0 || trend == Trend::Falling) {
        SectionLabel::Outro
    } else if trend == Trend::Rising {
        SectionLabel::Build
    } else if avg_rms < -34.0 || avg_onsets < 0.6 {
        SectionLabel::Break
    } else if avg_onsets >= 1.5 {
        SectionLabel::Groove
    } else {
        SectionLabel::Plateau
    }
}

fn infer_tempo(
    frames: &[RawFeatureFrame],
    duration_ms: u64,
    evidence: &EvidenceMap,
) -> TempoUnderstanding {
    let avg_onsets = average(frames.iter().map(|frame| frame.onset_rate_per_sec));
    if frames.is_empty() || avg_onsets < 0.75 || duration_ms == 0 {
        return TempoUnderstanding {
            estimated_bpm: None,
            beat_grid: Vec::new(),
            beat_grid_summary: None,
            confidence: 0.2,
            fallback: Some(
                "rhythmic onsets are too sparse or weak for a stable BPM estimate".to_string(),
            ),
            evidence_ids: evidence_ids(evidence, &[EvidenceFeature::Rhythm]),
        };
    }

    let mut bpm = avg_onsets * 60.0 * 1.1;
    while bpm < MIN_BEAT_BPM {
        bpm *= 2.0;
    }
    while bpm > MAX_BEAT_BPM {
        bpm *= 0.5;
    }
    let confidence = confidence_from_level(avg_onsets, 0.75, 3.0).max(0.3);
    let interval_ms = (60_000.0 / bpm).round().max(1.0) as u64;
    let beat_count = duration_ms / interval_ms + 1;
    let explicit_count = (beat_count as usize).min(MAX_EXPLICIT_BEAT_MARKERS);
    let mut beat_grid = Vec::with_capacity(explicit_count);
    for beat_index in 0..explicit_count {
        let position_ms = interval_ms.saturating_mul(beat_index as u64);
        let kind = if beat_index == 0 {
            BeatMarkerKind::Downbeat
        } else if beat_index % 16 == 0 {
            BeatMarkerKind::Phrase
        } else if beat_index % 4 == 0 {
            BeatMarkerKind::Bar
        } else {
            BeatMarkerKind::Beat
        };
        beat_grid.push(BeatMarker {
            position_ms,
            kind,
            confidence,
        });
    }
    let explicit_grid_end_ms = beat_grid.last().map(|beat| beat.position_ms).unwrap_or(0);

    TempoUnderstanding {
        estimated_bpm: Some((bpm * 10.0).round() / 10.0),
        beat_grid,
        beat_grid_summary: Some(BeatGridSummary {
            interval_ms,
            beat_count,
            grid_start_ms: 0,
            grid_end_ms: duration_ms,
            truncated: beat_count as usize > explicit_count,
        }),
        confidence,
        fallback: if beat_count as usize > explicit_count {
            Some(format!(
                "explicit beat_grid is capped at {MAX_EXPLICIT_BEAT_MARKERS} markers; use beat_grid_summary interval/count to extend deterministically through {duration_ms} ms (last explicit marker {explicit_grid_end_ms} ms)"
            ))
        } else {
            None
        },
        evidence_ids: evidence_ids(evidence, &[EvidenceFeature::Rhythm]),
    }
}

fn infer_moods(frames: &[RawFeatureFrame], evidence: &EvidenceMap) -> Vec<MoodEstimate> {
    if frames.is_empty() {
        return Vec::new();
    }
    let avg_centroid = average(frames.iter().map(|frame| frame.spectral_centroid_hz));
    let avg_flatness = average(frames.iter().map(|frame| frame.spectral_flatness));
    let avg_rms = average(frames.iter().map(|frame| frame.rms_db));
    let avg_zcr = average(frames.iter().map(|frame| frame.zero_crossing_rate));
    let trend = dominant_trend(frames.iter().map(|frame| frame.trend));
    let mut moods = Vec::new();

    if avg_centroid >= 3_000.0 {
        moods.push(mood(
            MoodDescriptor::Bright,
            0.7,
            evidence,
            &[EvidenceFeature::Spectrum],
        ));
    } else if avg_centroid >= 300.0 {
        moods.push(mood(
            MoodDescriptor::Warm,
            0.6,
            evidence,
            &[EvidenceFeature::Spectrum],
        ));
    }
    if avg_flatness >= 0.45 || avg_zcr >= 0.18 {
        moods.push(mood(
            MoodDescriptor::Dense,
            0.58,
            evidence,
            &[EvidenceFeature::Texture],
        ));
    } else {
        moods.push(mood(
            MoodDescriptor::Sparse,
            0.52,
            evidence,
            &[EvidenceFeature::Texture],
        ));
    }
    if avg_rms >= -18.0 && avg_centroid >= 2_000.0 {
        moods.push(mood(
            MoodDescriptor::Tense,
            0.55,
            evidence,
            &[EvidenceFeature::Dynamics, EvidenceFeature::Spectrum],
        ));
    } else if avg_rms < -22.0 {
        moods.push(mood(
            MoodDescriptor::Settled,
            0.55,
            evidence,
            &[EvidenceFeature::Dynamics],
        ));
    }
    match trend {
        Trend::Rising => moods.push(mood(
            MoodDescriptor::Rising,
            0.56,
            evidence,
            &[EvidenceFeature::Trend],
        )),
        Trend::Falling => moods.push(mood(
            MoodDescriptor::Falling,
            0.56,
            evidence,
            &[EvidenceFeature::Trend],
        )),
        Trend::Stable => {}
    }

    moods
}

fn mood(
    descriptor: MoodDescriptor,
    confidence: f32,
    evidence: &EvidenceMap,
    features: &[EvidenceFeature],
) -> MoodEstimate {
    MoodEstimate {
        descriptor,
        confidence,
        evidence_ids: evidence_ids(evidence, features),
    }
}

fn infer_components(
    frames: &[RawFeatureFrame],
    evidence: &EvidenceMap,
    rhythm_confidence: f32,
) -> Vec<ComponentEstimate> {
    if frames.is_empty() {
        return Vec::new();
    }
    let avg_onsets = average(frames.iter().map(|frame| frame.onset_rate_per_sec));
    let avg_centroid = average(frames.iter().map(|frame| frame.spectral_centroid_hz));
    let avg_flatness = average(frames.iter().map(|frame| frame.spectral_flatness));
    let avg_zcr = average(frames.iter().map(|frame| frame.zero_crossing_rate));
    let avg_rms = average(frames.iter().map(|frame| frame.rms_db));
    let mut components = Vec::new();

    if avg_onsets >= 1.0 {
        components.push(component(
            MusicalComponent::Drums,
            rhythm_confidence.max(0.45),
            evidence,
            &[EvidenceFeature::Rhythm],
        ));
    }
    if avg_centroid < 700.0 && avg_rms > -42.0 {
        components.push(component(
            MusicalComponent::Bass,
            0.48,
            evidence,
            &[EvidenceFeature::Spectrum, EvidenceFeature::Dynamics],
        ));
    }
    if avg_onsets < 1.0 && avg_flatness < 0.35 && avg_rms > -45.0 {
        components.push(component(
            MusicalComponent::Pad,
            0.44,
            evidence,
            &[EvidenceFeature::Spectrum, EvidenceFeature::Texture],
        ));
    }
    if (700.0..4_000.0).contains(&avg_centroid) && avg_flatness < 0.45 {
        components.push(component(
            MusicalComponent::Lead,
            0.42,
            evidence,
            &[EvidenceFeature::Spectrum],
        ));
    }
    if (1_000.0..5_000.0).contains(&avg_centroid) && (0.04..0.18).contains(&avg_zcr) {
        components.push(component(
            MusicalComponent::VocalLike,
            0.35,
            evidence,
            &[EvidenceFeature::Spectrum, EvidenceFeature::Texture],
        ));
    }
    if avg_flatness >= 0.55 {
        components.push(component(
            MusicalComponent::NoiseTexture,
            avg_flatness.clamp(0.4, 0.85),
            evidence,
            &[EvidenceFeature::Texture],
        ));
    }

    components
}

fn component(
    component: MusicalComponent,
    confidence: f32,
    evidence: &EvidenceMap,
    features: &[EvidenceFeature],
) -> ComponentEstimate {
    ComponentEstimate {
        component,
        confidence,
        evidence_ids: evidence_ids(evidence, features),
    }
}

fn summarize_flow(
    sections: &[AudioSection],
    frames: &[RawFeatureFrame],
    evidence: &EvidenceMap,
) -> FlowUnderstanding {
    let labels: Vec<String> = sections
        .iter()
        .map(|section| format!("{:?}", section.label).to_lowercase())
        .collect();
    let trend = dominant_trend(frames.iter().map(|frame| frame.trend));
    let contour = match trend {
        Trend::Rising => "overall rising energy",
        Trend::Falling => "overall falling energy",
        Trend::Stable => "mostly stable energy",
    };
    let summary = if labels.is_empty() {
        "no stable sections detected; analysis fell back to global descriptors".to_string()
    } else {
        format!("{} with {}", labels.join(" -> "), contour)
    };

    FlowUnderstanding {
        summary,
        confidence: if sections.len() > 1 { 0.62 } else { 0.4 },
        evidence_ids: evidence_ids(
            evidence,
            &[EvidenceFeature::Dynamics, EvidenceFeature::Trend],
        ),
    }
}

fn evidence_ids(evidence: &EvidenceMap, features: &[EvidenceFeature]) -> Vec<String> {
    evidence
        .entries
        .iter()
        .filter(|entry| features.contains(&entry.feature))
        .map(|entry| entry.id.clone())
        .collect()
}

fn average(values: impl IntoIterator<Item = f32>) -> f32 {
    let mut count = 0usize;
    let mut sum = 0.0_f32;
    for value in values {
        if value.is_finite() {
            count += 1;
            sum += value;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}

fn dominant_trend(values: impl IntoIterator<Item = Trend>) -> Trend {
    let mut rising = 0usize;
    let mut falling = 0usize;
    for trend in values {
        match trend {
            Trend::Rising => rising += 1,
            Trend::Falling => falling += 1,
            Trend::Stable => {}
        }
    }
    if rising > falling && rising > 0 {
        Trend::Rising
    } else if falling > rising && falling > 0 {
        Trend::Falling
    } else {
        Trend::Stable
    }
}

fn confidence_from_level(value: f32, weak: f32, strong: f32) -> f32 {
    if !value.is_finite() || strong <= weak {
        return 0.0;
    }
    ((value - weak) / (strong - weak)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const SAMPLE_RATE: u32 = 48_000;

    fn analyzer() -> OfflineAudioUnderstandingAnalyzer {
        OfflineAudioUnderstandingAnalyzer::new(AudioUnderstandingConfig::default())
    }

    #[test]
    fn semantic_understanding_serializes_without_action_hints() {
        let samples = sine_samples(440.0, 4.0, 0.35);
        let understanding = analyzer().analyze_mono_samples("fixture-tone", &samples, SAMPLE_RATE);

        assert!(!understanding.raw_features.frames.is_empty());
        assert!(!understanding.evidence.entries.is_empty());
        assert!(!understanding.sections.is_empty());
        assert!(understanding.flow.confidence > 0.0);
        assert!(understanding
            .moods
            .iter()
            .any(|mood| matches!(mood.descriptor, MoodDescriptor::Warm)));

        let json = serde_json::to_string(&understanding).expect("serialize understanding");
        let lowered = json.to_lowercase();
        for banned in [
            "recommendation",
            "transition choice",
            "sketch hint",
            "glicol",
        ] {
            assert!(
                !lowered.contains(banned),
                "serialized understanding leaked {banned}: {json}"
            );
        }
    }

    #[test]
    fn shared_contract_fixture_matches_rust_schema() {
        let fixture_json =
            include_str!("../../../agent/src/analysis/fixtures/audio-understanding-v1.json");
        let fixture: SemanticAudioUnderstanding =
            serde_json::from_str(fixture_json).expect("shared fixture deserializes");

        assert_eq!(fixture.schema_version, AUDIO_UNDERSTANDING_SCHEMA_VERSION);
        assert_eq!(
            fixture.asset_id,
            "fixture://deterministic-four-section-loop"
        );
        assert_eq!(fixture.raw_features.frames.len(), 4);
        assert_eq!(fixture.sections.len(), 4);
        assert_eq!(fixture.tempo.estimated_bpm, Some(120.0));
        assert!(fixture
            .components
            .iter()
            .any(|component| component.component == MusicalComponent::Drums));

        let rust_value = serde_json::to_value(&fixture).expect("fixture serializes");
        let round_tripped: SemanticAudioUnderstanding =
            serde_json::from_value(rust_value).expect("serialized fixture round-trips");
        assert_eq!(round_tripped, fixture);
    }

    #[test]
    fn click_train_estimates_bpm_and_beat_grid() {
        let samples = click_train(120.0, 8.0);
        let understanding =
            analyzer().analyze_mono_samples("fixture-clicks", &samples, SAMPLE_RATE);

        let bpm = understanding.tempo.estimated_bpm.expect("bpm estimate");
        assert!((110.0..=130.0).contains(&bpm), "bpm {bpm}");
        assert!(understanding.tempo.confidence >= 0.45);
        assert!(understanding.tempo.fallback.is_none());
        let summary = understanding
            .tempo
            .beat_grid_summary
            .as_ref()
            .expect("beat grid summary");
        assert_eq!(summary.grid_start_ms, 0);
        assert_eq!(summary.grid_end_ms, understanding.duration_ms);
        assert!(!summary.truncated);
        assert!(summary.beat_count as usize >= understanding.tempo.beat_grid.len());
        assert!(understanding.tempo.beat_grid.len() >= 8);
        assert_eq!(
            understanding.tempo.beat_grid[0].kind,
            BeatMarkerKind::Downbeat
        );
        assert!(understanding
            .components
            .iter()
            .any(|component| component.component == MusicalComponent::Drums));
    }

    #[test]
    fn long_tracks_keep_compact_beat_grid_metadata_when_explicit_grid_is_capped() {
        let samples = click_train(120.0, 360.0);
        let understanding =
            analyzer().analyze_mono_samples("fixture-long-clicks", &samples, SAMPLE_RATE);
        let summary = understanding
            .tempo
            .beat_grid_summary
            .as_ref()
            .expect("beat grid summary");

        assert_eq!(
            understanding.tempo.beat_grid.len(),
            MAX_EXPLICIT_BEAT_MARKERS
        );
        assert!(summary.truncated);
        assert_eq!(summary.grid_end_ms, understanding.duration_ms);
        assert!(summary.beat_count as usize > understanding.tempo.beat_grid.len());
        assert!(understanding
            .tempo
            .fallback
            .as_deref()
            .unwrap_or_default()
            .contains("capped"));
    }

    #[test]
    fn weak_rhythm_uses_explicit_tempo_fallback() {
        let samples = sine_samples(220.0, 4.0, 0.2);
        let understanding = analyzer().analyze_mono_samples("fixture-pad", &samples, SAMPLE_RATE);

        assert!(understanding.tempo.estimated_bpm.is_none());
        assert!(understanding.tempo.fallback.is_some());
        assert!(understanding.tempo.confidence <= 0.3);
    }

    #[test]
    fn path_analysis_guard_rejects_inputs_beyond_configured_duration() {
        let err = validate_analysis_frame_count((SAMPLE_RATE as usize) * 3, SAMPLE_RATE, 2_000)
            .expect_err("duration guard should reject inputs above the configured limit");

        match err {
            AudioUnderstandingError::InputTooLong {
                duration_ms,
                max_duration_ms,
            } => {
                assert_eq!(duration_ms, 3_000);
                assert_eq!(max_duration_ms, 2_000);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn section_change_points_follow_energy_shift() {
        let mut samples = sine_samples(220.0, 4.0, 0.08);
        samples.extend(click_train(120.0, 4.0));
        let understanding =
            analyzer().analyze_mono_samples("fixture-change", &samples, SAMPLE_RATE);

        assert!(
            understanding.sections.len() >= 2,
            "sections: {:?}",
            understanding.sections
        );
        assert!(understanding
            .sections
            .iter()
            .any(|section| section.label == SectionLabel::Intro
                || section.label == SectionLabel::Break));
        assert!(understanding
            .sections
            .iter()
            .any(|section| section.label == SectionLabel::Groove
                || section.label == SectionLabel::Build));
    }

    fn sine_samples(freq_hz: f32, seconds: f32, amp: f32) -> Vec<f32> {
        let len = (SAMPLE_RATE as f32 * seconds) as usize;
        (0..len)
            .map(|index| (TAU * freq_hz * index as f32 / SAMPLE_RATE as f32).sin() * amp)
            .collect()
    }

    fn click_train(bpm: f32, seconds: f32) -> Vec<f32> {
        let len = (SAMPLE_RATE as f32 * seconds) as usize;
        let interval = (SAMPLE_RATE as f32 * 60.0 / bpm) as usize;
        (0..len)
            .map(|index| if index % interval < 192 { 0.95 } else { 0.0 })
            .collect()
    }
}
