import { readFileSync } from "node:fs";

export type RawFeatureFrame = {
  window_start_ms: number;
  window_duration_ms: number;
  peak_db: number;
  rms_db: number;
  crest_factor: number;
  trend: "Stable" | "Rising" | "Falling";
  spectral_centroid_hz: number;
  spectral_rolloff_hz: number;
  spectral_flatness: number;
  onset_rate_per_sec: number;
  zero_crossing_rate: number;
  energy_label: "Silent" | "Quiet" | "Moderate" | "Loud" | "Peak";
  brightness_label: "Dark" | "Warm" | "Neutral" | "Bright";
  texture_label: "Tonal" | "Mixed" | "Noisy";
};

export type EvidenceEntry = {
  id: string;
  feature: "Dynamics" | "Spectrum" | "Rhythm" | "Texture" | "Trend";
  frame_range: {
    start_index: number;
    end_index: number;
  };
  summary: string;
  strength: number;
};

export type AudioSection = {
  start_ms: number;
  end_ms: number;
  label: "Intro" | "Groove" | "Build" | "Break" | "Plateau" | "Outro";
  confidence: number;
  evidence_ids: string[];
};

export type BeatMarker = {
  position_ms: number;
  kind: "Beat" | "Downbeat" | "Bar" | "Phrase";
  confidence: number;
};

export type AudioUnderstandingFixture = {
  schema_version: 1;
  asset_id: string;
  duration_ms: number;
  raw_features: {
    asset_id: string;
    sample_rate: number;
    duration_ms: number;
    frames: RawFeatureFrame[];
  };
  evidence: {
    entries: EvidenceEntry[];
  };
  sections: AudioSection[];
  flow: {
    summary: string;
    confidence: number;
    evidence_ids: string[];
  };
  tempo: {
    estimated_bpm: number | null;
    beat_grid: BeatMarker[];
    beat_grid_summary: {
      interval_ms: number;
      beat_count: number;
      grid_start_ms: number;
      grid_end_ms: number;
      truncated: boolean;
    } | null;
    confidence: number;
    fallback: string | null;
    evidence_ids: string[];
  };
  moods: Array<{
    descriptor:
      | "Bright"
      | "Warm"
      | "Dense"
      | "Sparse"
      | "Tense"
      | "Settled"
      | "Rising"
      | "Falling";
    confidence: number;
    evidence_ids: string[];
  }>;
  components: Array<{
    component: "Drums" | "Bass" | "Pad" | "Lead" | "VocalLike" | "NoiseTexture";
    confidence: number;
    evidence_ids: string[];
  }>;
};

export const canonicalAudioUnderstandingFixtureJson = readFileSync(
  new URL("./audio-understanding-v1.json", import.meta.url),
  "utf8",
).trim();

export const canonicalAudioUnderstandingFixture = JSON.parse(
  canonicalAudioUnderstandingFixtureJson,
) as AudioUnderstandingFixture;
