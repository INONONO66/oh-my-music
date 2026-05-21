# oh-my-music Architecture

> AI Music Harness — LLM이 실시간으로 오디오를 분석하고, 믹싱하고, 생성하는 시스템

**Status**: Draft v0.1
**Date**: 2026-04-28

---

## Table of Contents

1. [Overview](#1-overview)
2. [Sound Sources](#2-sound-sources)
3. [Tech Stack](#3-tech-stack)
4. [Project Structure](#4-project-structure)
5. [Rust Engine Architecture](#5-rust-engine-architecture)
6. [DSP Chain & Effects](#6-dsp-chain--effects)
7. [Glicol Integration](#7-glicol-integration)
8. [IPC Protocol](#8-ipc-protocol)
9. [Agent Architecture](#9-agent-architecture-bunts)
10. [LLM Tool Definitions](#10-llm-tool-definitions)
11. [Interface Layer](#11-interface-layer)
12. [Data Flow](#12-data-flow)
13. [Safety](#13-safety)
14. [Milestones](#14-milestones)

---

## 1. Overview

oh-my-music는 AI 에이전트가 실시간 오디오 파이프라인을 자율적으로 제어하는 "AI Music Harness"다.

**핵심 루프:**

```
[소리 입력] → [Rust 엔진: 캡처/DSP/믹싱] → [스피커 or 디코 보이스]
                      ↑                              │
                      │ (명령)                        │ (분석 데이터)
                      │                              ↓
                [Bun 에이전트: 분석 → LLM 판단 → 도구 호출]
```

**설계 원칙:**

1. **Rust가 오디오 시계를 소유한다** — 오디오 콜백은 절대 lock, alloc, log, await하지 않는다
2. **LLM은 고수준 판단만** — "에너지 올려", "리버브 깊게" → 목표값 설정. 실시간 보간은 Rust
3. **환경에 반응한다** — 카페면 활발하게, 조용하면 차분하게, 유저가 말하면 음악 줄이기
4. **사람이 안 건드려도 돌아간다** — 자율 DJ 모드

---

## 2. Sound Sources

oh-my-music는 4가지 사운드 소스를 믹싱한다:

| Source | 설명 | 캡처 방법 |
|--------|------|----------|
| **System Audio** | 현재 시스템에서 재생 중인 모든 소리 (Spotify, YouTube 등) | Core Audio Tap (macOS 14.2+) |
| **Microphone** | 유저 목소리 + 주변 환경음 (카페 소음 등) | CPAL input stream |
| **Selected Song** | 유저가 선택한 로컬 음악 파일 (MP3, WAV, FLAC) | 파일 디코딩 → 버퍼 |
| **Coded Sound (Glicol)** | LLM이 Glicol 코드를 생성하여 만든 프로그래매틱 사운드 | Glicol Engine (Rust 네이티브) |


### 2.1 Timeline Source Instance Model

The runtime now has a protocol-level dynamic source model above the current fixed `SourceId` channels. Each audible stream is represented as a `TimelineSourceInstance` with:

- stable `source_instance_id` such as `legacy:mic` or a future scheduled layer id
- `source_kind`: `System`, `Mic`, `File`, or `Generated`
- optional `asset_ref` for live input, file URI/hash/duration, or generated engine/code reference
- one or more `active_windows` with `timeline_start_ms`, optional `timeline_end_ms`, and `source_start_offset_ms`
- `playback` status (`Pending`, `Queued`, `Playing`, `Paused`, `Stopped`, `Ended`, `Failed`) plus an authority marker (`TimelineTransport` vs `LegacyChannelEnabled`)
- current per-source effect status: gain, pan, HPF/LPF, EQ, reverb send, playback rate, reverse flag
- optional `legacy_bridge` that maps an existing fixed `SourceId` channel into the dynamic model
- validation contract for non-empty/namespace-safe ids and non-empty, sorted, non-overlapping active windows

```text
SourceId::Player channel
  → SourceInstanceId("legacy:player")
  → SourceKind::File
  → TimelineSourceInstance
  → SourceTimelineSnapshot
```

PR2 establishes the schema and fixed-channel bridge only. The bridge reports existing channel enablement with `PlaybackStatusAuthority::LegacyChannelEnabled`, not as future scheduler transport authority. `AudioRuntime::source_timeline_snapshot()` is a non-real-time status adapter and must not run inside the render callback. Later PRs attach planned scheduling, dynamic file/generated layer allocation, source playback controls, and per-source effect automation to these source instances through validated handlers/adapters.

### Source 특성

**System Audio:**
- Core Audio Process Tap으로 캡처
- 자기 자신(oh-my-music)의 출력은 제외하여 피드백 방지
- 기본 gain: -6 dB (클리핑 방지)

**Microphone:**
- 환경음 분석 (YAMNet): 카페/조용/야외/음악/소음 분류
- 유저 음성 감지 → 사이드체인 덕킹 (음악 자동으로 줄임)
- 기본 HPF: 80Hz, 기본 gain: -12 dB, 기본 mute

**Selected Song:**
- symphonia 크레이트로 디코딩 (MP3, WAV, FLAC, OGG, AAC)
- 48kHz 스테레오로 리샘플링
- BPM/key 분석 결과를 에이전트에 전달
- 트랜스포트 제어: play, pause, seek, loop

**Coded Sound (Glicol):**
- LLM이 Glicol DSL 코드를 생성
- `Engine::update_with_code()`로 핫스왑 (음악 끊김 없이 패턴 변경)
- `Engine::next_block()`으로 매 블록 f32 샘플 획득
- 시스템 오디오나 마이크를 `~input`으로 받아서 이펙트 체인도 가능
- 빌트인 신스: SinOsc, SawOsc, SquOsc, TriOsc
- 빌트인 드럼: Bd (킥), Sn (스네어), Hh (하이햇)
- 빌트인 이펙트: Plate (리버브), Pan, Delay, Filter
- 빌트인 시퀀서: Sequencer, Speed, Choose, Arrange

---

## 3. Tech Stack

| Layer | Technology | Why |
|-------|-----------|-----|
| **Audio Engine** | Rust (CPAL + Core Audio Tap + Glicol) | 실시간 안전성, <5ms 레이턴시, 오디오 생태계 |
| **IPC** | Unix Domain Socket + MessagePack | 로컬 전용, 낮은 오버헤드, length-prefixed |
| **Agent** | Bun + TypeScript + [Pi SDK](https://pi.dev/) (`@mariozechner/pi-coding-agent`) | 세션 기반 LLM 에이전트, 도구 호출, 이벤트 스트리밍, 15+ 모델 프로바이더 |
| **Audio Analysis** | Meyda (JS) + essentia.js (WASM) + TF.js (YAMNet) | BPM/key/에너지 + 환경음 분류 |
| **Storage** | Bun SQLite (built-in) | 유저 프로파일, 결정 로그, 패턴 저장 |
| **TUI** | Ink (React for CLI) | 스펙트럼, 미터, 에이전트 상태, 명령 입력 |
| **Discord** | discord.js + @discordjs/voice | 보이스 채널 출력, 슬래시 커맨드 |

---

## 4. Project Structure

```
oh-my-music/
├── Cargo.toml                          # Rust workspace
├── crates/
│   ├── omm-engine/                     # 바이너리: CPAL I/O, Core Audio Tap, IPC 서버
│   │   └── src/
│   │       ├── main.rs
│   │       ├── ipc_server.rs           # UDS 서버, 메시지 디코딩
│   │       ├── session.rs              # 오디오 세션 관리
│   │       ├── config.rs               # 엔진 설정
│   │       └── runtime.rs              # 오디오 런타임 오케스트레이션
│   │
│   ├── omm-audio/                      # 라이브러리: DSP, 믹서, 소스, 분석
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── graph.rs                # 오디오 그래프 (고정 토폴로지)
│   │       ├── mixer.rs                # 소스 → 버스 → 마스터 믹싱
│   │       ├── transport.rs            # BPM, 박자, 재생 상태
│   │       ├── analysis_tap.rs         # 분석용 링 버퍼 쓰기
│   │       │
│   │       ├── source/
│   │       │   ├── system.rs           # Core Audio Tap 캡처
│   │       │   ├── mic.rs              # CPAL 마이크 입력
│   │       │   ├── player.rs           # 파일 재생 (symphonia)
│   │       │   └── glicol.rs           # Glicol 엔진 래퍼
│   │       │
│   │       ├── dsp/
│   │       │   ├── smoothing.rs        # 파라미터 보간
│   │       │   ├── eq.rs               # 3-band + parametric EQ
│   │       │   ├── filters.rs          # HPF, LPF, BPF, filter sweep
│   │       │   ├── dynamics.rs         # compressor, limiter, gate
│   │       │   ├── reverb.rs           # plate reverb
│   │       │   ├── delay.rs            # delay, ping-pong
│   │       │   ├── saturation.rs       # saturation, bitcrusher
│   │       │   ├── modulation.rs       # chorus, phaser, flanger, tremolo
│   │       │   ├── spatial.rs          # pan, stereo width
│   │       │   ├── ducking.rs          # 사이드체인 덕킹
│   │       │   ├── time.rs             # time stretch, pitch shift
│   │       │   ├── stutter.rs          # stutter, beat repeat, reverse, tape stop
│   │       │   └── meters.rs           # RMS, peak, spectrum
│   │       │
│   │       └── output/
│   │           ├── cpal_out.rs         # 로컬 스피커 출력
│   │           └── discord_sink.rs     # Discord PCM/Opus 프레임
│   │
│   └── omm-protocol/                   # 라이브러리: IPC 메시지 정의
│       └── src/
│           ├── lib.rs
│           ├── envelope.rs             # 메시지 봉투
│           ├── messages.rs             # 전체 메시지 타입
│           ├── params.rs               # 파라미터 ID, 범위, 검증
│           └── validation.rs           # 범위 클램프, 안전 검증
│
├── agent/                              # Bun + TypeScript
│   ├── package.json
│   ├── tsconfig.json
│   └── src/
│       ├── index.ts                    # 엔트리포인트
│       ├── ipc.ts                      # UDS 클라이언트 (MessagePack)
│       ├── engine-client.ts            # 엔진 명령 추상화
│       │
│       ├── analysis/
│       │   ├── pipeline.ts             # 분석 파이프라인 오케스트레이션
│       │   ├── meyda.ts               # spectral centroid, flux, MFCC, RMS
│       │   ├── essentia.ts            # BPM, key detection
│       │   ├── yamnet.ts              # 환경음 분류 (TensorFlow.js)
│       │   └── features.ts            # FeatureStore (롤링 윈도우)
│       │
│       ├── agent/
│       │   ├── decision.ts            # LLM 결정 루프 (매 ~2초)
│       │   ├── extension.ts           # Pi SDK Extension (registerTool 진입점)
│       │   ├── tools/
│       │   │   ├── energy.ts          # set_energy, set_mood
│       │   │   ├── source.ts          # set_source_level, set_source_mute, player_control
│       │   │   ├── dsp.ts             # EQ, filter, reverb, delay, dynamics, ducking
│       │   │   ├── glicol.ts          # create_pattern, modify_pattern, stop_pattern
│       │   │   ├── transition.ts      # trigger_transition
│       │   │   └── utility.ts         # load_preset, emergency_fade, reset_mix 등
│       │   ├── context.ts             # LLM context 빌더
│       │   └── safety.ts              # 도구 호출 검증, rate limit
│       │
│       ├── profile/
│       │   ├── store.ts               # SQLite 프로파일 저장소
│       │   └── schema.ts              # DB 스키마
│       │
│       ├── interface/
│       │   ├── tui/
│       │   │   ├── app.tsx            # Ink 루트 컴포넌트
│       │   │   ├── meters.tsx         # 레벨 미터
│       │   │   ├── spectrum.tsx       # 스펙트럼 시각화
│       │   │   ├── status.tsx         # 엔진/캡처 상태
│       │   │   ├── agent-log.tsx      # LLM 결정 로그
│       │   │   └── input.tsx          # 명령 입력
│       │   │
│       │   └── discord/
│       │       ├── bot.ts             # Discord 봇 엔트리포인트
│       │       ├── voice.ts           # 보이스 채널 연결, PCM 스트리밍
│       │       ├── commands.ts        # 슬래시 커맨드 정의
│       │       └── ducking.ts         # 디코 음성 → 덕킹 신호
│       │
│       └── pattern/
│           ├── generator.ts           # LLM → Glicol 코드 생성
│           └── templates.ts           # 장르별 Glicol 템플릿
│
└── docs/
    ├── ARCHITECTURE.md                # 이 문서
    └── adr/                           # Architecture Decision Records
```

---

## 5. Rust Engine Architecture

### 5.1 Internal Audio Format

```rust
pub const ENGINE_SAMPLE_RATE: u32 = 48_000;
pub const ENGINE_CHANNELS: usize = 2;
pub const MAX_BLOCK_FRAMES: usize = 512;
pub const DISCORD_FRAME_FRAMES: usize = 960; // 20ms @ 48kHz
```

- 내부 처리: `f32`, stereo, `[-1.0, 1.0]`
- 엔진 샘플레이트: 48kHz
- Discord 출력: 48kHz stereo s16le, 960 frames/20ms
- 로컬 출력: CPAL 디바이스 포맷으로 엣지에서만 변환

### 5.2 Audio Thread Rules

오디오 콜백에서 **절대 하면 안 되는 것:**

- Lock (mutex, rwlock)
- Allocate (Vec::push, Box::new, String)
- Log (println!, tracing)
- Await (async)
- SQLite 접근
- MessagePack 디코딩
- LLM 호출
- Vec 리사이즈
- 샘플 로딩
- Glicol 코드 컴파일 (`update_with_code`)
- Unbounded 큐 사용

### 5.3 Callback Structure

```rust
fn render_callback(output: &mut [StereoFrame], ctx: &mut AudioRuntime) {
    let frames = output.len();

    // 1. 커맨드 드레인 (lock-free 큐에서 최대 64개)
    ctx.drain_rt_commands(MAX_COMMANDS_PER_BLOCK);

    // 2. 소스 풀링
    ctx.pull_system_audio(frames);      // Core Audio Tap 링 버퍼에서
    ctx.pull_mic_input(frames);         // CPAL 입력 링 버퍼에서
    ctx.pull_player_audio(frames);      // 파일 재생 버퍼에서
    ctx.pull_glicol_output(frames);     // Glicol Engine::next_block()

    // 3. Per-source DSP
    ctx.process_per_source_dsp(frames);

    // 4. 믹싱
    ctx.mix_sources_to_buses(frames);

    // 5. FX send/return
    ctx.process_fx_returns(frames);

    // 6. 마스터 체인
    ctx.process_master_chain(frames);

    // 7. 안전 장치
    ctx.safety_limiter.process(output);
    ctx.nan_guard_and_clamp(output);

    // 8. 분석 탭 (non-blocking write to ring buffer)
    ctx.write_analysis_taps(output);
    ctx.update_atomic_meters(output);
}
```

### 5.4 Timing Constraints

| Item | Target |
|------|--------|
| 샘플레이트 | 48kHz |
| CPAL 요청 버퍼 | 128 or 256 frames |
| 최대 콜백 블록 | 512 frames |
| 콜백 CPU 목표 | 버퍼 시간의 <25% |
| 커맨드 드레인 캡 | 64개/블록 |
| 분석 쓰기 | non-blocking only |
| 기본 스무딩 | 200ms |
| 최대 뮤지컬 스무딩 | 2s |

### 5.5 Lock-Free Command Queue

```rust
pub struct RtCommand {
    pub seq: u64,
    pub target: RtTarget,
    pub param: ParamId,
    pub value: f32,
    pub ramp_frames: u32,
}

pub enum RtTarget {
    Master,
    Source(SourceId),
    Bus(BusId),
    Fx(FxId),
}

const RT_COMMAND_QUEUE_CAPACITY: usize = 1024;
const MAX_COMMANDS_PER_BLOCK: usize = 64;
```

- IPC 스레드가 범위 검증 후 enqueue
- 오디오 스레드가 한 번 더 clamp
- 큐 풀이면 `Nack { code: QueueFull }` 반환
- 안전 명령(EmergencyFade, Mute)은 별도 emergency lane 예약

### 5.6 Analysis Ring Buffer

오디오 콜백은 per-channel 분석용 링 버퍼에 mono f32 다운믹스 샘플만 쓴다. 별도 non-RT 분석 스레드가 버퍼를 drain하여 `ChannelFeatures` 스냅샷을 계산한다. 현재 구현의 안정 계약은 다음과 같다.

| Layer | Format | Cadence / Window | Purpose |
|-------|--------|------------------|---------|
| `ChannelStrip` analysis tap | mono f32, engine sample rate | render block마다 non-blocking push | RT 스레드에서 분석 샘플 복사 |
| `FeatureAnalyzerHandle` | mono f32, per source channel | 2s window, 1024-sample hop | peak/RMS, trend, centroid, rolloff, flatness, onset rate, zero-crossing rate, coarse labels |
| Offline audio-understanding fixture/schema | JSON-safe feature frames + evidence map | deterministic asset-level frames | sections/flow, BPM/beat, mood/feel, component estimates with confidence |

링 버퍼 용량은 2초 분석 윈도우의 두 배와 hop 여유분을 포함하도록 샘플레이트에서 계산한다. Producer가 가득 차면 현재 샘플은 RT 안전을 위해 버려질 수 있으므로, 향후 public analysis stream에는 overflow counter를 추가해야 한다.

현재 Audio Understanding 경계는 분석/증거 표현까지다. 출력은 DJ action recommendation, transition choice, generated-code sketch hint를 포함하지 않는다.

### 5.7 Core Audio Tap

```
Core Audio Process Tap
  → Aggregate/private tap device
  → IOProc callback
  → f32 stereo 변환
  → timestamp 정규화
  → lock-free system input ring
```

요구사항:
- oh-my-music 프로세스 자체를 탭에서 제외 (피드백 방지)
- mach timebase로 호스트 시간 → 나노초 변환
- 채널 레이아웃 → 스테레오 변환 (mono: 복제, multi: 다운믹스)
- 샘플레이트 ≠ 48kHz면 콜백 밖에서 리샘플

---

## 6. DSP Chain & Effects

### 6.1 Signal Chain Order

**Per Source:**
```
Input
  → DC Blocker
  → High-pass Filter
  → Noise Gate (마이크만)
  → Gain Trim
  → 3-Band EQ (Low/Mid/High)
  → Compressor
  → Saturation
  → Pan / Stereo Width
  → FX Sends (Reverb, Delay)
  → Source Fader
  → Bus
```

**FX Returns:**
```
Delay Return → Delay Filter → Return Gain
Reverb Return → Return Gain
  → FX Return Bus
```

**Master:**
```
Bus Sum
  → Master Gain
  → Master EQ
  → Glue Compressor
  → Stereo Width
  → Safety Limiter
  → Clamp / Format Conversion
```

### 6.2 Effects — Full List

#### Phase 1 (MVP)

**Mixing Fundamentals:**
| Effect | Parameters | Range | Default |
|--------|-----------|-------|---------|
| Gain | `gain_db` | -60..+12 dB | 0 dB |
| Pan | `pan` | -1..+1 | 0 |
| Crossfade | `position` | 0..1 | — |
| Mute/Solo | `mute`, `solo` | bool | false |

**EQ:**
| Effect | Parameters | Range | Default |
|--------|-----------|-------|---------|
| EQ Low Shelf | `gain_db` | -12..+12 dB | 0 |
| EQ Mid Peak | `gain_db` | -12..+12 dB | 0 |
| EQ High Shelf | `gain_db` | -12..+12 dB | 0 |
| High-pass Filter | `freq_hz` | 20..1000 Hz | source별 |
| Low-pass Filter | `freq_hz` | 1000..20000 Hz | 20000 |
| Filter Sweep | `freq_hz` + `ramp_ms` | — | — |

**Spatial:**
| Effect | Parameters | Range | Default |
|--------|-----------|-------|---------|
| Reverb (Plate) | `size`, `decay_sec`, `damping`, `pre_delay_ms`, `send_db` | 0..1, 0.2..10, 0..1, 0..200, -60..0 | 0.4, 2.0, 0.5, 20, -inf |
| Delay | `time_ms`, `feedback`, `send_db` | 20..2000, 0..0.85, -60..0 | 375, 0.25, -inf |

**Dynamics:**
| Effect | Parameters | Range | Default |
|--------|-----------|-------|---------|
| Compressor | `threshold_db`, `ratio`, `attack_ms`, `release_ms`, `makeup_db`, `mix` | -60..0, 1..20, 0.1..100, 10..1000, -12..+12, 0..1 | -18, 2, 10, 150, 0, 1 |
| Limiter | `ceiling_dbfs`, `lookahead_ms`, `release_ms` | -6..-0.1, 1..10, 20..500 | -1.0, 5, 100 |
| Gate | `threshold_db`, `attack_ms`, `release_ms` | -80..-20, 1..50, 20..1000 | -50, 10, 200 |
| Sidechain Ducking | `amount_db`, `attack_ms`, `release_ms`, `threshold_dbfs` | 0..24, 5..100, 80..1500, -70..-20 | 6, 20, 300, -42 |

**Analysis (이펙트 아님, 필수 유틸):**
| Feature | Method | Window |
|---------|--------|--------|
| Spectrum Analyzer | FFT | 2048 samples |
| Level Meter (RMS/Peak) | 직접 계산 | 50ms |

#### Phase 2

| Effect | 설명 |
|--------|------|
| Parametric EQ | 특정 주파수/Q 지정 |
| Band-pass Filter | 특정 대역만 통과 |
| Chorus | 약간 어긋난 복사본으로 두꺼운 소리 |
| Phaser | 위상 변화 "휘~" 소리 |
| Flanger | 제트기 소리 효과 |
| Saturation | 따뜻한 아날로그 색감 |
| Bitcrusher | 해상도 낮춤 (로파이) |
| Time Stretch | 피치 유지하고 속도 변경 |
| Pitch Shift | 속도 유지하고 피치 변경 |
| Stutter / Beat Repeat | 짧은 구간 반복 |
| BPM Detection | Essentia 기반 |
| Key Detection | Essentia 기반 |

#### Phase 3

| Effect | 설명 |
|--------|------|
| Reverse | 거꾸로 재생 |
| Tape Stop | 느려지면서 피치 다운 |
| Ping-pong Delay | 좌→우 에코 |
| Waveshaper | 파형 변형 |
| Stereo Widener | 스테레오 폭 조절 |
| Auto-pan | 자동 좌우 이동 |
| Tremolo | 볼륨 흔들림 |
| Vibrato | 피치 흔들림 |
| LFO → any param | 범용 모듈레이션 |

### 6.3 Parameter Smoothing

모든 외부 제어 파라미터는 스무딩을 거친다:

```rust
pub struct SmoothedParam {
    current: f32,
    target: f32,
    step: f32,
    remaining_frames: u32,
}
```

| 상황 | Ramp |
|------|------|
| 기본 | 200ms |
| LLM 뮤지컬 변경 | 500ms..2000ms |
| 긴급 뮤트/페이드 | 20ms..100ms |
| 템포/패턴 밀도 | 비트/바 경계에 맞춤 |

### 6.4 Mixer Design

**Bus 구조:**
```rust
pub enum BusId {
    SystemBus,
    MicBus,
    GeneratedBus,    // Glicol + Player 합산
    FxReturnBus,
    MasterBus,
}
```

**기본 게인:**
| Source | Default Gain | Notes |
|--------|-------------|-------|
| System | -6 dB | 캡처된 음악 클리핑 방지 |
| Mic | -12 dB | 기본 mute, 명시적 활성화 필요 |
| Player | -6 dB | 파일 재생 |
| Glicol | -9 dB | 헤드룸 확보 |
| FX Return | -12 dB | 보수적 |
| Master | -3 dB | 리미터 전 헤드룸 |

**믹싱 공식:**
```
master_in =
    system_bus × system_fader
  + mic_bus × mic_fader
  + generated_bus × generated_fader
  + fx_return_bus
```

**사이드체인 덕킹:**
- TUI: 마이크 엔벨로프가 system/generated 덕킹
- Discord: 유저 음성 활동이 봇 음악 출력 덕킹
- Discord 유저 음성은 절대 봇 출력에 재전송하지 않음

---

## 7. Glicol Integration

### 7.1 Glicol이란

[Glicol](https://github.com/chaosprint/glicol)은 Rust 네이티브 라이브 코딩 오디오 언어. Graph 기반 DSP를 DSL로 정의하고 실시간으로 핫스왑 가능.

### 7.2 왜 Glicol인가 (vs Strudel)

| | Glicol | Strudel |
|--|--------|---------|
| 런타임 | **Rust 네이티브** — 같은 프로세스 | Node.js 별도 프로세스 |
| 오디오 출력 | `&[f32]` 슬라이스 직접 반환 | Web Audio API (브라우저) |
| 외부 입력 | `~input` 노드로 외부 오디오 수신 가능 | 제한적 |
| 핫스왑 | LCS 디핑으로 무중단 코드 교체 | 가능 |
| IPC 불필요 | 동일 프로세스 → 제로 오버헤드 | UDS/WebSocket 필요 |
| 빌트인 노드 | 40+ (오실레이터, 필터, 드럼, 시퀀서) | 더 풍부하지만 JS 의존 |

### 7.3 Embedding API

```rust
use glicol::Engine;

pub struct GlicolSource {
    engine: Engine<256>,  // 256-sample 블록
}

impl GlicolSource {
    pub fn new() -> Self {
        let mut engine = Engine::<256>::new();
        engine.set_sr(48000);
        engine.set_bpm(120.0);
        Self { engine }
    }

    /// LLM이 생성한 코드 로드 (non-RT 스레드에서 호출)
    pub fn load_code(&mut self, code: &str) -> Result<(), EngineError> {
        self.engine.update_with_code(code)
    }

    /// 매 블록 호출 (오디오 콜백에서)
    pub fn process(&mut self, frames: usize) -> &[Buffer<256>] {
        self.engine.next_block(vec![])
    }

    /// 외부 오디오를 Glicol에 통과시킬 때
    pub fn process_with_input(&mut self, left: &[f32], right: &[f32]) -> &[Buffer<256>] {
        self.engine.next_block(vec![left, right])
    }

    /// 실시간 파라미터 변경 (오디오 콜백에서 안전)
    pub fn set_param(&mut self, chain: &str, pos: usize, param: u8, value: f32) {
        self.engine.send_msg(&format!("{},{},{},{}", chain, pos, param, value));
    }

    pub fn set_bpm(&mut self, bpm: f32) {
        self.engine.set_bpm(bpm);
    }
}
```

### 7.4 Glicol DSL 예시 (LLM이 생성할 코드)

```rust
// 로파이 힙합 비트
"
~drums: choose 100 >> sp \bd >> mul 0.8
~hh: choose 80 >> sp \hh >> mul 0.3
~bass: saw 55 >> lpf 200 1.0 >> mul 0.5
out: mix ~drums ~hh ~bass >> plate 0.3
"

// 앰비언트 패드
"
~pad: sin 220 >> mul 0.3
~pad2: sin 330 >> mul 0.2
out: mix ~pad ~pad2 >> plate 0.8 >> lpf 2000 0.5
"

// 시스템 오디오에 이펙트 적용
"
out: ~input >> lpf 800 0.8 >> plate 0.6
"
```

### 7.5 주의사항

- `update_with_code()`는 파싱 + 그래프 리빌드 포함 → **오디오 콜백 밖에서 호출**
- `next_block()`과 `send_msg()`는 오디오 콜백에서 호출 가능
- 샘플은 `'static` 수명 필요 → 사전 로딩 또는 leak
- crates.io 미게시 → git dependency 사용
- 버전 0.14.0-dev → API 변경 가능성

### 7.6 Glicol 코드 안전성

LLM이 생성한 Glicol 코드는 반드시 검증:

1. `update_with_code()` 에러 시 이전 코드 유지 (engine은 자동 롤백)
2. 출력 게인 클램프: Glicol 출력도 리미터 통과
3. 무한 피드백 방지: delay/reverb feedback 최대 0.85
4. 코드 길이 제한: 최대 4096자
5. 노드 수 제한: 최대 64개

---

## 8. IPC Protocol

### 8.1 Transport

Unix Domain Socket, length-prefixed MessagePack.

```
[u32 little-endian payload length][MessagePack payload]
```

소켓 경로: `$XDG_RUNTIME_DIR/oh-my-music/engine.sock`
폴백: `/tmp/oh-my-music-${uid}/engine.sock`

### 8.2 Envelope

```typescript
type Envelope<T> = {
  v: 1;                    // 프로토콜 버전
  msgId: bigint;           // 고유 메시지 ID
  corrId?: bigint;         // 요청-응답 연관 ID
  sessionId: string;       // 오디오 세션 ID
  source: "engine" | "agent" | "tui" | "discord";
  kind: string;            // 메시지 타입 이름
  priority: "realtime" | "normal" | "telemetry";
  sentAtNs: bigint;        // monotonic 나노초 타임스탬프
  body: T;
};
```

### 8.3 Agent → Engine Messages

The table below separates implemented protocol rows from planned contract direction. PR2 adds dynamic source timeline schema/event types and a fixed-channel bridge; runtime scheduling/command handlers remain planned for later PRs.

| Kind | Purpose | Status |
|------|---------|--------|
| `Hello` | 핸드셰이크 | implemented |
| `StartSession` | 모드(tui/discord) + 출력 라우트 설정 | implemented |
| `StopSession` | 페이드아웃 후 세션 종료 | implemented |
| `SetCapture` | 시스템 오디오/마이크 캡처 on/off | implemented |
| `SetParam` | 단일 파라미터 변경 | implemented |
| `SetParamBatch` | 복수 파라미터 일괄 변경 (LLM 결정 1회분) | implemented |
| `SetSourceMute` | 소스 mute 상태 변경 | implemented |
| `GlicolLoadCode` | Glicol DSL 코드 로드 | implemented |
| `EmergencyFade` | 긴급 페이드아웃 | implemented |
| `RequestState` | 현재 엔진 상태 요청 | implemented |
| `GlicolSetParam` | Glicol 런타임 파라미터 변경 | planned |
| `PlayerLoad` | 음악 파일 로드 | planned |
| `PlayerControl` | play/pause/seek/loop | planned |
| `ResetDsp` | DSP 체인 초기화 | planned |
| `Subscribe` | 스트림 구독 (meters, analysis, discord-pcm) | planned |

### 8.4 Engine → Agent Messages

| Kind | Purpose | Status |
|------|---------|--------|
| `HelloAck` | 핸드셰이크 응답 (capabilities, sample rate) | implemented |
| `Ack` | 명령 성공 | implemented |
| `Nack` | 명령 거부 (코드 + 사유) | implemented |
| `StateSnapshot` | 전체 엔진 상태 덤프 | implemented |
| `MeterFrame` | RMS/peak/gain reduction (10-15 FPS) | implemented |
| `ChannelFeatures` / future `AnalysisPcmChunk` | 현재 Rust feature snapshot 또는 향후 PCM 분석 청크 (PR1은 아직 IPC 이벤트를 추가하지 않고 오디오 이해 스키마/오프라인 분석 기반만 정의) | planned |
| `DiscordPcmFrame` | Discord 출력 프레임 (20ms, 3840 bytes) | planned |
| `CaptureStatus` | 캡처 상태 변경 알림 | implemented |
| `SourceTimelineSnapshot` | dynamic source instances with active windows/playback/effect status | schema implemented / emission planned |
| `XRunEvent` | 오디오 드롭아웃 알림 | planned |

### 8.5 Reconnection

- 하트비트: 매 1000ms
- 3000ms 응답 없으면 stale
- 현재 재연결 시: Hello → HelloAck → StateSnapshot
- 계획된 구독 재연결 흐름: Hello → HelloAck → Subscribe(when implemented) → StateSnapshot
- 에이전트 연결 끊김 시: 오디오는 마지막 안전 상태로 계속 재생, 30초 후 생성 레이어 페이드아웃

---

## 9. Agent Architecture (Bun/TS + Pi SDK)

### 9.1 Pi SDK Overview

[Pi](https://pi.dev/) (`@mariozechner/pi-coding-agent`)는 세션 기반 AI 에이전트 프레임워크.

**왜 Pi인가:**
- **세션 기반** — 상태(메시지 히스토리, 도구 결과)가 자동 관리됨. Vercel AI SDK는 stateless
- **이벤트 스트리밍** — `text_delta`, `tool_execution_start/end` 등 세밀한 이벤트
- **`steer()` / `followUp()`** — 실행 중 인터럽트 또는 후속 프롬프트 큐잉
- **15+ 모델 프로바이더** — Anthropic, OpenAI, Google, Groq, DeepSeek 등 런타임 전환 가능
- **Extension 시스템** — `pi.registerTool()`로 커스텀 도구 등록
- **세션 컴팩션** — 긴 세션에서 자동/수동 컨텍스트 요약

```typescript
import { createAgentSession, defineTool, SessionManager } from "@mariozechner/pi-coding-agent";
import { getModel, Type } from "@mariozechner/pi-ai";

const { session } = await createAgentSession({
  model: getModel("anthropic", "claude-sonnet-4"),
  customTools: [setEnergyTool, setMoodTool, setEqTool, ...],
  sessionManager: SessionManager.inMemory(),
});
```

### 9.2 Decision Loop

매 ~2초마다 실행. LLM 호출이 겹치지 않도록 보장.

```
Every ~2s:
  1. FeatureStore에서 최근 2초 + 10초 요약 수집
  2. 컨텍스트 빌드 (현재 상태 + 환경 + 유저 프로파일 + 최근 결정)
  3. Pi session.prompt()로 LLM 호출 (tool calling)
  4. 도구의 execute() 내에서 SafetyPolicy 검증
  5. SetParamBatch로 합쳐서 Rust 엔진에 전송
  6. 결정 로그 SQLite에 저장
```

유저 명령이 들어오면:
- 구조화 커맨드 (`/gain`, `/mute`) → LLM 바이패스, 직접 엔진 명령
- 자연어 ("좀 더 차분하게") → `session.steer()` (실행 중이면 인터럽트) 또는 `session.followUp()` (대기 후 실행)

### 9.3 Decision Loop Policy

| Setting | Value |
|---------|-------|
| 결정 간격 | 2000ms |
| 최대 동시 LLM 호출 | 1 |
| 이전 호출 실행 중이면 | 사이클 스킵 |
| 사이클당 최대 도구 호출 | 8 |
| 사이클당 최대 게인 증가 | +3 dB |
| 최대 마스터 게인 | 0 dB |
| 기본 커맨드 ramp | 200..2000ms |

### 9.4 LLM Context

```typescript
type LlmContext = {
  mode: "tui" | "discord";
  userGoal: string | null;

  currentState: {
    energy: number;           // 0..1
    bpm?: number;
    key?: string;
    masterPeakDb: number;
    limiterGainReductionDb: number;
    activeSources: string[];
    glicol: {
      codeLoaded: boolean;
      activeChains: string[];
    };
    player: {
      playing: boolean;
      trackName?: string;
      positionSec?: number;
    };
  };

  environment: {
    label: "quiet" | "cafe" | "outdoor" | "speech" | "music" | "noise";
    confidence: number;
    speechProbability: number;
    noiseFloorDb: number;
  };

  recentFeatures: {
    last2s: FeatureSummary;
    last10s: FeatureSummary;
    trend: "rising" | "falling" | "stable";
  };

  userPreferences: object;
  recentUserCommands: string[];  // 최근 5개
  recentDecisions: Array<{       // 최근 5개
    at: string;
    summary: string;
    tools: string[];
  }>;

  safety: {
    clipping: boolean;
    queuePressure: number;      // 0..1
    captureStatus: object;
  };
};
```

**LLM에 포함하지 않는 것:** raw PCM, 전체 미터 히스토리, 스택 트레이스, 전체 Glicol 코드 (패턴 편집 작업 아닌 한)

### 9.5 Analysis Pipeline

Current foundation:

```text
Audio source / ChannelStrip
  → mono f32 analysis tap
  → FeatureAnalyzerHandle non-RT thread
  → ChannelFeatures snapshots
       ├─ dynamics: peak_db, rms_db, crest_factor, trend
       ├─ spectrum: centroid, rolloff, flatness
       ├─ rhythm texture: onset_rate_per_sec, zero_crossing_rate
       └─ coarse labels: energy, brightness, texture
```

The asset-level Audio Understanding schema sits above raw feature frames:

```text
RawFeatureTimeline
  → EvidenceMap keyed by evidence ids
  → SemanticAudioUnderstanding
       ├─ sections / flow summary
       ├─ BPM, projected metrical beat/downbeat/bar/phrase markers with compact beat-grid summary/truncation metadata
       ├─ mood / feel labels linked to evidence
       └─ component estimates linked to evidence
```

Required boundary: the semantic object explains audio content and evolution only. It must not include DJ action recommendations, generated-code sketch hints, “best transition” choices, or unsupported subjective claims. Beat/downbeat/bar/phrase markers in PR1 are projected from the estimated beat interval; they are not yet structural downbeat/bar/phrase detections.

Future richer analysis may add a PCM chunk router and JS/WASM processors:

```
Engine AnalysisPcmChunk
  → chunk router (tap별 분배)
  ├─ Meyda: spectral centroid, flux, MFCC, RMS (50ms hop)
  ├─ Essentia.js: BPM (8-16s window), key (12s window)
  └─ TF.js YAMNet: 환경 분류 (0.96s @ 16kHz)
       │
       ↓
  FeatureStore (rolling windows)
       │
       ↓
  compact summary → LLM context
```

**YAMNet 히스테리시스:**
- 환경 레이블 변경 전 연속 3개 윈도우 확인
- 10초 롤링 분포 유지
- 일시적 소리(잔 깨지는 소리 등)로 믹스 대변화 방지

### 9.6 SQLite Schema

```sql
PRAGMA journal_mode = WAL;

-- 유저 프로파일
CREATE TABLE profiles (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,            -- local-user | discord-user | guild
  discord_user_id TEXT,
  discord_guild_id TEXT,
  preferred_energy REAL DEFAULT 0.5,
  volume_limit_db REAL DEFAULT -1.0,
  favorite_styles TEXT DEFAULT '[]',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

-- 세션 로그
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  mode TEXT NOT NULL,
  profile_id TEXT,
  guild_id TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT
);

-- LLM 결정 로그
CREATE TABLE decisions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  context_json TEXT NOT NULL,
  tool_calls_json TEXT NOT NULL,
  rationale TEXT
);

-- 저장된 Glicol 패턴
CREATE TABLE patterns (
  id TEXT PRIMARY KEY,
  profile_id TEXT,
  name TEXT NOT NULL,
  code TEXT NOT NULL,
  tags TEXT DEFAULT '[]',
  created_at TEXT NOT NULL
);
```

---

## 10. LLM Tool Definitions

Pi SDK의 `defineTool()` + TypeBox 스키마로 정의. 모든 도구의 `execute()`에서 SafetyPolicy 검증 → SetParamBatch 변환을 거친다.

### 도구 정의 패턴

```typescript
import { defineTool, type ExtensionContext } from "@mariozechner/pi-coding-agent";
import { Type } from "@mariozechner/pi-ai";

// 예시: setEnergy 도구
const setEnergyTool = defineTool({
  name: "set_energy",
  label: "Set Energy",
  description: "전체 믹스의 에너지 레벨을 조절한다. EQ, compression, gain, 패턴 밀도 등을 복합 조정.",
  parameters: Type.Object({
    targetEnergy: Type.Number({ minimum: 0, maximum: 1, description: "목표 에너지 (0=calm, 1=max)" }),
    rampMs: Type.Number({ minimum: 500, maximum: 5000, description: "전환 시간 (ms)" }),
    reason: Type.String({ description: "변경 사유" }),
  }),
  executionMode: "sequential",
  execute: async (toolCallId, params, signal, onUpdate, ctx) => {
    const batch = safetyPolicy.validate(energyToParamBatch(params));
    await engineClient.sendParamBatch(batch);
    return {
      content: [{ type: "text", text: `Energy → ${params.targetEnergy} (${params.rampMs}ms)` }],
      details: { applied: batch.commands.length },
    };
  },
});
```

### 고수준 의도 도구

```typescript
set_energy({ targetEnergy, rampMs, reason })
// → EQ brightness, compression, gain, 패턴 밀도 등 복합 조정

set_mood({ mood, intensity, rampMs })
// mood: "calm" | "focus" | "energetic" | "dark" | "bright" | "dreamy" | "minimal"
// → 프리셋 기반 복합 파라미터 조정
```

### 소스 제어 도구

```typescript
set_source_level({ source, gainDb, rampMs })
set_source_mute({ source, muted, rampMs })
player_control({ action: "play" | "pause" | "seek" | "next", seekSec? })
```

### DSP 제어 도구

```typescript
set_three_band_eq({ target, lowDb, midDb, highDb, rampMs })
set_filter({ target, highpassHz?, lowpassHz?, resonance?, rampMs })
set_reverb({ target, sendDb, size, decaySec, damping, rampMs })
set_delay({ target, sendDb, time, feedback, rampMs })
set_dynamics({ target, thresholdDb, ratio, attackMs, releaseMs, makeupDb, mix })
set_ducking({ trigger, target, amountDb, attackMs, releaseMs })
```

### Glicol 제어 도구

```typescript
create_pattern({ prompt, style, bpm?, key?, constraints: { maxLayers, intensity } })
// → LLM이 Glicol DSL 생성 → engine에 GlicolLoadCode

modify_pattern({ instruction, preserveGroove, transitionMs })
// instruction: "베이스 더 깊게"

stop_pattern({ fadeMs })
```

### 트랜지션 도구

```typescript
trigger_transition({
  type: "crossfade" | "filter-sweep" | "breakdown" | "build" | "drop" | "fade-out",
  durationMs,     // 500..16000
  intensity        // 0..1
})
```

### 유틸리티 도구

```typescript
load_preset({ preset: "safe-default" | "focus" | "ambient" | "party", rampMs })
save_user_preference({ key, value, scope })
send_interface_message({ target, message, severity })
emergency_fade({ reason })
reset_mix({ preset: "safe-default", rampMs })
```

### 도구 등록 (Extension)

```typescript
// agent/src/agent/extension.ts
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";

export default function ommExtension(pi: ExtensionAPI) {
  // 고수준
  pi.registerTool(setEnergyTool);
  pi.registerTool(setMoodTool);

  // 소스
  pi.registerTool(setSourceLevelTool);
  pi.registerTool(setSourceMuteTool);
  pi.registerTool(playerControlTool);

  // DSP
  pi.registerTool(setThreeBandEqTool);
  pi.registerTool(setFilterTool);
  pi.registerTool(setReverbTool);
  pi.registerTool(setDelayTool);
  pi.registerTool(setDynamicsTool);
  pi.registerTool(setDuckingTool);

  // Glicol
  pi.registerTool(createPatternTool);
  pi.registerTool(modifyPatternTool);
  pi.registerTool(stopPatternTool);

  // 트랜지션
  pi.registerTool(triggerTransitionTool);

  // 유틸리티
  pi.registerTool(loadPresetTool);
  pi.registerTool(saveUserPreferenceTool);
  pi.registerTool(sendInterfaceMessageTool);
  pi.registerTool(emergencyFadeTool);
  pi.registerTool(resetMixTool);
}
```

---

## 11. Interface Layer

### 11.1 TUI

Ink (React for CLI) 기반. 10-15 FPS 렌더링.

```
┌ oh-my-music ──────────────────────────────────────────────┐
│ Mode: TUI  Engine: OK  Capture: System OK / Mic OK        │
│ BPM: 124  Key: Am  Env: cafe  Energy: 0.63                │
├ Sources ──────────────────────────────────────────────────┤
│ System   ███████████░░░ -12.4 dB                          │
│ Mic      ███░░░░░░░░░░░ -32.1 dB  [muted]                │
│ Player   ██████████░░░░ -8.2 dB   ♫ lo-fi-beats.mp3      │
│ Glicol   ████████░░░░░░ -16.0 dB  [3 chains active]      │
│ Master   ██████████░░░░ -3.2 dB   Limiter GR: 1.4 dB     │
├ Spectrum ─────────────────────────────────────────────────┤
│ ▁▂▃▅▇▆▅▄▃▂▃▄▆▇▅▃▂▁                                      │
├ Agent ────────────────────────────────────────────────────┤
│ Goal: energetic but speech-friendly                       │
│ Last: lowered highs, added 6dB ducking (1.2s ago)         │
│ Glicol: ~drums: choose 100 >> sp \bd >> mul 0.8           │
├ Input ────────────────────────────────────────────────────┤
│ > make it warmer                                          │
└───────────────────────────────────────────────────────────┘
```

**지원 명령:**

자연어:
```
make it more energetic
reduce the mic
make it less harsh
add some lo-fi drums
```

구조화 명령 (LLM 바이패스):
```
/gain system -6
/mute mic
/duck generated 10
/preset focus
/play ~/music/track.mp3
/glicol "out: sin 440 >> mul 0.3"
/stop
/reset
```

### 11.2 Discord Bot

**보이스 채널 오디오 경로:**
```
Rust engine
  → 48kHz stereo s16le PCM, 20ms frames (3840 bytes)
  → UDS DiscordPcmFrame
  → Bun Readable stream
  → @discordjs/voice AudioResource(StreamType.Raw)
  → @discordjs/opus encoder
  → Discord voice
```

**텍스트/슬래시 커맨드:**
```
/omm start          # 보이스 채널 입장
/omm stop           # 보이스 채널 퇴장
/omm mood energetic # 분위기 변경
/omm play <url>     # 트랙 재생
/omm duck 10db      # 덕킹 설정
@oh-my-music 좀 더 차분하게 해줘    # 자연어
```

**디코 유저 음성 덕킹:**
```
Discord voice receive
  → per-user Opus decode / packet energy
  → speech activity estimate
  → 사이드체인 덕킹 커맨드
  → Rust 엔진: 음악 자동으로 줄임
```

**Multi-guild (나중에):**
- v1: 엔진 프로세스 1개 = Discord 세션 1개
- 스케일: guild별 별도 엔진 프로세스 스폰

---

## 12. Data Flow

### 12.1 TUI Mode

```
System apps ─── Core Audio Tap ──→ ┐
Microphone  ─── CPAL input ──────→ ├─→ Rust Engine ──→ CPAL speakers
Music file  ─── symphonia ───────→ │     │    ↑
Glicol code ─── Engine ──────────→ ┘     │    │
                                         │    │ SetParamBatch
                                         ↓    │ GlicolLoadCode
                                    Analysis   │
                                    chunks     │
                                         ↓    │
                                    Bun Agent ─┘
                                     ├─ Meyda
                                     ├─ Essentia
                                     ├─ YAMNet
                                     ├─ LLM (2s loop)
                                     └─ TUI (Ink)
```

### 12.2 Discord Mode

```
Discord slash/text commands
         │
         ↓
  Bun Discord bot
         │
         ↓
  Agent command router
         │
         ↓
  LLM decision loop
         │
         ↓
  Engine commands (UDS)
         │
         ↓
  Rust Engine
   ├─ Player source
   ├─ Glicol source
   ├─ DSP chain
   └─ 48kHz s16le PCM frames
         │
         ↓
  Bun Readable stream
         │
         ↓
  @discordjs/voice + Opus
         │
         ↓
  Discord voice channel
```

### 12.3 LLM Decision Flow

```
FeatureStore (2s summary)
  + engine state
  + environment label
  + user profile
  + recent commands
  + safety state
         │
         ↓
  Pi session.prompt() (tool calling)
         │
         ↓
  Tool calls (max 8/cycle)
         │
         ↓
  SafetyPolicy validation
  (clamp, rate limit, gain cap)
         │
         ↓
  SetParamBatch / GlicolLoadCode
         │
         ↓
  Rust engine command queue
         │
         ↓
  Smoothed DSP changes (200ms~2s)
```

---

## 13. Safety

### 13.1 Always-On Safety

| Layer | Rule |
|-------|------|
| Parameter validation | Bun과 Rust 양쪽에서 검증 |
| Output clamp | 최종 `[-1.0, 1.0]` 클램프 |
| NaN guard | non-finite 샘플 → `0.0` 대체 |
| Master limiter | 항상 활성 |
| Feedback cap | delay/reverb feedback 최대 0.85 |
| Gain cap | LLM이 마스터를 0 dB 이상 올릴 수 없음 |
| Emergency fade | -60 dB over 100ms |
| Queue overflow | 오디오 스레드 절대 블록 안 함 |
| Watchdog | xrun/dropout을 콜백 밖에서 보고 |
| Glicol code length | 최대 4096자 |
| Glicol node count | 최대 64개 |

### 13.2 Limiter Defaults

| Parameter | Default | Range |
|-----------|---------|-------|
| Ceiling | -1.0 dBFS | -6..-0.1 |
| Lookahead | 5ms | 1..10 |
| Release | 100ms | 20..500 |
| Max GR | 18 dB | 6..36 |

### 13.3 LLM Rate Limiting

| Constraint | Value |
|-----------|-------|
| 결정 간격 | 2s |
| 사이클당 게인 증가 상한 | +3 dB |
| 마스터 게인 상한 | 0 dB |
| 사이클당 도구 호출 상한 | 8 |
| Glicol 코드 교체 쿨다운 | 5s |

---

## 14. Milestones

### Phase 1 — Audio Pipeline PoC (2-3주)

- [ ] Rust workspace 셋업 (omm-engine, omm-audio, omm-protocol)
- [ ] CPAL output (로컬 스피커 재생)
- [ ] CPAL input (마이크 캡처)
- [ ] Core Audio Tap (시스템 오디오 캡처)
- [ ] Glicol 통합 (코드 → 사운드 → 믹서)
- [ ] 기본 DSP 체인 (EQ, HPF/LPF, Reverb, Limiter)
- [ ] 믹서 (4 소스 → 마스터)
- [ ] 분석 탭 (링 버퍼 → 청크)
- [ ] UDS IPC 서버 (MessagePack)

### Phase 2 — Agent + TUI (2-3주)

- [ ] Bun 에이전트: IPC 클라이언트
- [ ] 분석 파이프라인 (Meyda + YAMNet)
- [ ] Pi SDK 도구 정의 (defineTool + Extension)
- [ ] LLM 결정 루프 (2s)
- [ ] SQLite 프로파일/로그
- [ ] TUI (Ink): 미터, 스펙트럼, 상태, 명령 입력
- [ ] 구조화 커맨드 (/gain, /mute, /preset)
- [ ] 자연어 커맨드 → LLM

### Phase 3 — Discord + 확장 (2-3주)

- [ ] Discord 봇 + 보이스 채널 연결
- [ ] PCM 스트리밍 (Rust → Bun → Discord)
- [ ] 슬래시 커맨드
- [ ] 유저 음성 덕킹
- [ ] Essentia.js BPM/key detection
- [ ] Phase 2 이펙트 (Chorus, Phaser, Saturation, Bitcrusher)
- [ ] 파일 재생 (symphonia)
- [ ] Glicol 템플릿 (장르별)

### Phase 4 — Polish (ongoing)

- [ ] Phase 3 이펙트 (Reverse, Tape Stop, LFO, Auto-pan)
- [ ] Time stretch / Pitch shift
- [ ] Multi-guild Discord
- [ ] 유저 프로파일 학습
- [ ] Rust-side Opus 인코딩
- [ ] 성능 최적화

---

## Appendix: Key Dependencies

### Rust

```toml
[dependencies]
cpal = { version = "0.15", features = ["audio_thread_priority"] }
glicol = { git = "https://github.com/chaosprint/glicol.git" }
glicol_synth = { git = "https://github.com/chaosprint/glicol.git" }
rtrb = "0.3"                    # lock-free ring buffer
rmp-serde = "1"                 # MessagePack
serde = { version = "1", features = ["derive"] }
symphonia = "0.5"               # audio file decoding
tokio = { version = "1", features = ["net", "rt-multi-thread"] }
```

### Bun/TypeScript

```json
{
  "dependencies": {
    "@mariozechner/pi-coding-agent": "latest",
    "@mariozechner/pi-ai": "latest",
    "@mariozechner/pi-agent-core": "latest",
    "meyda": "latest",
    "essentia.js": "latest",
    "@tensorflow/tfjs": "latest",
    "@msgpack/msgpack": "latest",
    "ink": "latest",
    "ink-text-input": "latest",
    "react": "latest",
    "discord.js": "latest",
    "@discordjs/voice": "latest",
    "@discordjs/opus": "latest"
  }
}
```
