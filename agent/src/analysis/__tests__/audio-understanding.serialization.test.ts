import { describe, expect, test } from "bun:test";

import {
  canonicalAudioUnderstandingFixture,
  canonicalAudioUnderstandingFixtureJson,
  type AudioUnderstandingFixture,
} from "../fixtures/audio-understanding-fixture";

type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

const sortJsonValue = (value: JsonValue): JsonValue => {
  if (Array.isArray(value)) {
    return value.map(sortJsonValue);
  }

  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, nestedValue]) => [key, sortJsonValue(nestedValue)]),
    );
  }

  return value;
};

const stableStringify = (value: JsonValue): string =>
  JSON.stringify(sortJsonValue(value));

const sharedFixture = JSON.parse(
  canonicalAudioUnderstandingFixtureJson,
) as AudioUnderstandingFixture;

const collectEvidenceIds = (fixture: AudioUnderstandingFixture): string[] => [
  ...fixture.sections.flatMap((section) => section.evidence_ids),
  ...fixture.flow.evidence_ids,
  ...fixture.tempo.evidence_ids,
  ...fixture.moods.flatMap((mood) => mood.evidence_ids),
  ...fixture.components.flatMap((component) => component.evidence_ids),
];

const collectKeys = (value: JsonValue): string[] => {
  if (!value || typeof value !== "object") {
    return [];
  }

  if (Array.isArray(value)) {
    return value.flatMap(collectKeys);
  }

  return Object.entries(value).flatMap(([key, nestedValue]) => [
    key,
    ...collectKeys(nestedValue),
  ]);
};

describe("audio understanding shared v1 fixture", () => {
  test("loads the shared Rust/TypeScript contract fixture deterministically", () => {
    expect(
      stableStringify(
        canonicalAudioUnderstandingFixture as unknown as JsonValue,
      ),
    ).toBe(stableStringify(sharedFixture as unknown as JsonValue));
  });

  test("round-trips through JSON without changing semantic content", () => {
    const serialized = stableStringify(
      canonicalAudioUnderstandingFixture as unknown as JsonValue,
    );
    const roundTripped = JSON.parse(serialized) as AudioUnderstandingFixture;

    expect(roundTripped).toEqual(
      JSON.parse(stableStringify(sharedFixture as unknown as JsonValue)),
    );
    expect(roundTripped.schema_version).toBe(1);
    expect(roundTripped.tempo.estimated_bpm).toBe(120);
    expect(Array.isArray(roundTripped.tempo.beat_grid)).toBe(true);
    expect(roundTripped.tempo.beat_grid.length).toBeGreaterThan(0);
    expect(roundTripped.tempo.beat_grid[0].kind).toBe("Downbeat");
    expect(roundTripped.tempo.beat_grid_summary).toEqual({
      interval_ms: 500,
      beat_count: 33,
      grid_start_ms: 0,
      grid_end_ms: 16000,
      truncated: false,
    });
    expect(roundTripped.sections.map((section) => section.label)).toEqual([
      "Intro",
      "Groove",
      "Break",
      "Build",
    ]);
  });


  test("keeps top-level and raw feature metadata synchronized", () => {
    expect(canonicalAudioUnderstandingFixture.asset_id).toBe(
      canonicalAudioUnderstandingFixture.raw_features.asset_id,
    );
    expect(canonicalAudioUnderstandingFixture.duration_ms).toBe(
      canonicalAudioUnderstandingFixture.raw_features.duration_ms,
    );
  });

  test("keeps every semantic label backed by known evidence and frames", () => {
    const evidenceIds = new Set(
      canonicalAudioUnderstandingFixture.evidence.entries.map((entry) => entry.id),
    );
    const frameIndexes = new Set(
      canonicalAudioUnderstandingFixture.raw_features.frames.map((_, index) => index),
    );

    for (const semanticEvidenceId of collectEvidenceIds(
      canonicalAudioUnderstandingFixture,
    )) {
      expect(evidenceIds.has(semanticEvidenceId)).toBe(true);
    }

    for (const evidence of canonicalAudioUnderstandingFixture.evidence.entries) {
      expect(evidence.frame_range.start_index).toBeGreaterThanOrEqual(0);
      expect(evidence.frame_range.end_index).toBeGreaterThanOrEqual(
        evidence.frame_range.start_index,
      );
      expect(frameIndexes.has(evidence.frame_range.start_index)).toBe(true);
      expect(frameIndexes.has(evidence.frame_range.end_index)).toBe(true);
    }
  });


  test("uses label enum values accepted by the Rust schema", () => {
    const energyLabels = new Set(["Silent", "Quiet", "Moderate", "Loud", "Peak"]);
    const brightnessLabels = new Set(["Dark", "Warm", "Neutral", "Bright"]);
    const textureLabels = new Set(["Tonal", "Mixed", "Noisy"]);

    for (const frame of canonicalAudioUnderstandingFixture.raw_features.frames) {
      expect(energyLabels.has(frame.energy_label)).toBe(true);
      expect(brightnessLabels.has(frame.brightness_label)).toBe(true);
      expect(textureLabels.has(frame.texture_label)).toBe(true);
    }
  });

  test("stays within the audio-understanding boundary", () => {
    const serialized = stableStringify(
      canonicalAudioUnderstandingFixture as unknown as JsonValue,
    ).toLowerCase();
    const keys = collectKeys(
      canonicalAudioUnderstandingFixture as unknown as JsonValue,
    ).map((key) => key.toLowerCase());

    for (const forbidden of [
      "dj",
      "glicol",
      "recommend",
      "sketch",
      "besttransition",
    ] as const) {
      expect(serialized.includes(forbidden)).toBe(false);
      expect(keys.some((key) => key.includes(forbidden))).toBe(false);
    }
  });
});
