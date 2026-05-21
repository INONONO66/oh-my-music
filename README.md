# oh-my-music

AI Music Harness for local and Discord audio control.

## Concept

oh-my-music is a music-focused AI harness. A Rust real-time audio engine owns capture, DSP, mixing, and output. A Bun/TypeScript Pi agent analyzes state and issues safe high-level music-control commands.

## Core Stack

- **Rust engine**: CPAL, Core Audio Tap, Glicol, DSP, mixer
- **Agent**: Bun + TypeScript + Pi SDK
- **IPC**: Unix Domain Socket + MessagePack
- **Interfaces**: TUI first, Discord bot later
- **Sound sources**: system audio, microphone, selected songs, coded Glicol sound

## Repository Layout

```txt
crates/
  omm-engine/      # daemon / executable runtime
  omm-audio/       # DSP, sources, mixer, output sinks
  omm-protocol/    # shared IPC messages and validation

agent/             # Bun + TypeScript Pi agent
docs/              # architecture docs and ADRs
```

## Current Status

Early scaffold. The architecture draft lives at [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). The current audio-understanding foundation includes deterministic schema fixtures and Rust per-channel feature snapshots for dynamics, spectral texture, onset rate, zero-crossing rate, and coarse labels.

## First Milestone

Build an AI-independent Rust music control core first:

1. Start `omm-engine`
2. Load/run Glicol test sound
3. Apply basic DSP targets
4. Expose status and control over IPC
5. Wrap controls as Pi tools

The AI harness comes after the CLI/daemon control surface is stable.
