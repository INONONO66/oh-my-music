use std::f32::consts::TAU;

use omm_protocol::{
    frames_for_duration_ms, ActionOrigin, PlaybackState, ScheduleValidationError,
    ScheduledActionId, SourceInstanceId,
};
use serde::{Deserialize, Serialize};

use crate::{
    AudioRuntime, AudioRuntimeConfig, FileSourceInstanceRequest, QueueFull, RtCommand,
    RtCommandScheduleRequest, RtCommandSchedulerError, RtSourceInstanceId, SourceInstanceError,
    StereoFrame,
};

const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_BLOCK_FRAMES: usize = 256;
const DEFAULT_SOURCE_DURATION_MS: u64 = 31_000;
const DEFAULT_PLANNED_DELAY_MS: u64 = 30_000;
const DECK_A_ID: &str = "file:demo-deck-a";
const DECK_B_ID: &str = "file:demo-deck-b";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineDjDemoConfig {
    pub sample_rate: u32,
    pub block_frames: usize,
    pub source_duration_ms: u64,
    pub planned_delay_ms: u64,
}

impl Default for TimelineDjDemoConfig {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            block_frames: DEFAULT_BLOCK_FRAMES,
            source_duration_ms: DEFAULT_SOURCE_DURATION_MS,
            planned_delay_ms: DEFAULT_PLANNED_DELAY_MS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineDjDemoReport {
    pub sample_rate: u32,
    pub block_frames: usize,
    pub source_count: usize,
    pub overlapping_sources_playing: usize,
    pub initial_peak: f32,
    pub immediate_peak: f32,
    pub planned_peak: f32,
    pub stopped_peak: f32,
    pub immediate_controls: DemoSourceEffects,
    pub planned_controls: DemoSourceEffects,
    pub stopped_source_state: PlaybackState,
    pub planned_too_soon_rejected: bool,
    pub planned_trigger_frame: u64,
    pub planned_applied_engine_frame: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DemoSourceEffects {
    pub gain_db: f32,
    pub pan: f32,
    pub highpass_hz: f32,
    pub lowpass_hz: f32,
    pub eq_low_db: f32,
    pub eq_mid_db: f32,
    pub eq_high_db: f32,
    pub reverb_send_db: f32,
    pub playback_rate: f32,
    pub reverse: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TimelineDjDemoError {
    #[error("source instance operation failed: {0}")]
    Source(#[from] SourceInstanceError),
    #[error("planned action scheduling failed: {0}")]
    Schedule(#[from] RtCommandSchedulerError),
    #[error("immediate demo command queue was full: {0}")]
    QueueFull(#[from] QueueFull),
    #[error("timeline DJ demo requires a non-zero sample rate")]
    InvalidSampleRate,
    #[error("timeline DJ demo requires a non-zero block size")]
    InvalidBlockSize,
    #[error("demo source {0} was missing from the timeline snapshot")]
    MissingSource(&'static str),
}

pub fn run_timeline_dj_demo(
    config: TimelineDjDemoConfig,
) -> Result<TimelineDjDemoReport, TimelineDjDemoError> {
    if config.sample_rate == 0 {
        return Err(TimelineDjDemoError::InvalidSampleRate);
    }
    if config.block_frames == 0 {
        return Err(TimelineDjDemoError::InvalidBlockSize);
    }

    let (mut runtime, mut queue, _features) = AudioRuntime::new(AudioRuntimeConfig {
        sample_rate: config.sample_rate,
    });

    let deck_a = SourceInstanceId::new(DECK_A_ID);
    let deck_b = SourceInstanceId::new(DECK_B_ID);
    let source_duration_ms = config
        .source_duration_ms
        .max(config.planned_delay_ms.saturating_add(1_000));
    let deck_a_bytes = stereo_sine_wav(config.sample_rate, source_duration_ms, 55.0, 0.45);
    let deck_b_bytes = stereo_sine_wav(config.sample_rate, source_duration_ms, 110.0, 0.35);

    runtime.add_file_source_instance(file_request(
        &deck_a,
        "mem://demo-deck-a.wav",
        deck_a_bytes,
        0,
    ))?;
    runtime.add_file_source_instance(file_request(
        &deck_b,
        "mem://demo-deck-b.wav",
        deck_b_bytes,
        250,
    ))?;

    let mut output = vec![StereoFrame::SILENCE; config.block_frames];
    runtime.render_block(&mut output);
    let initial_peak = peak(&output);
    let initial_snapshot = runtime.source_timeline_snapshot();
    let submitted_at_frame = initial_snapshot.engine_frame;
    let overlapping_sources_playing = initial_snapshot
        .sources
        .iter()
        .filter(|source| source.playback.state == PlaybackState::Playing)
        .count();

    enqueue_immediate_controls(&mut queue)?;

    let too_soon = runtime.schedule_rt_command(RtCommandScheduleRequest {
        action_id: ScheduledActionId::new("demo-too-soon"),
        origin: ActionOrigin::PlannedLlm,
        trigger_frame: submitted_at_frame
            + frames_for_duration_ms(
                config.planned_delay_ms.saturating_sub(1),
                config.sample_rate,
            ),
        command: RtCommand::SetSourceInstanceGainDb {
            source_instance_id: RtSourceInstanceId::new(DECK_B_ID),
            db: -24.0,
            ramp_frames: 0,
        },
    });
    let planned_too_soon_rejected = match too_soon {
        Err(RtCommandSchedulerError::Validation(
            ScheduleValidationError::PlannedActionTooSoon { .. },
        )) => true,
        Ok(_) => false,
        Err(error) => return Err(TimelineDjDemoError::Schedule(error)),
    };

    let planned_trigger_frame =
        submitted_at_frame + frames_for_duration_ms(config.planned_delay_ms, config.sample_rate);
    runtime.schedule_rt_command(RtCommandScheduleRequest {
        action_id: ScheduledActionId::new("demo-planned-gain"),
        origin: ActionOrigin::PlannedLlm,
        trigger_frame: planned_trigger_frame,
        command: RtCommand::SetSourceInstanceGainDb {
            source_instance_id: RtSourceInstanceId::new(DECK_B_ID),
            db: -18.0,
            ramp_frames: 0,
        },
    })?;
    runtime.schedule_rt_command(RtCommandScheduleRequest {
        action_id: ScheduledActionId::new("demo-planned-eq"),
        origin: ActionOrigin::PlannedPi,
        trigger_frame: planned_trigger_frame,
        command: RtCommand::SetSourceInstanceEq {
            source_instance_id: RtSourceInstanceId::new(DECK_B_ID),
            low_db: -3.0,
            mid_db: 2.0,
            high_db: 6.0,
            ramp_frames: 0,
        },
    })?;

    runtime.render_block(&mut output);
    let immediate_peak = peak(&output);
    let immediate_snapshot = runtime.source_timeline_snapshot();
    let immediate_controls = effects_for(&immediate_snapshot, &deck_a, "deck A")?;

    while runtime.source_timeline_snapshot().engine_frame < planned_trigger_frame {
        runtime.render_block(&mut output);
    }
    runtime.render_block(&mut output);
    let planned_peak = peak(&output);
    let planned_snapshot = runtime.source_timeline_snapshot();
    let planned_controls = effects_for(&planned_snapshot, &deck_b, "deck B")?;

    // Deck A has finished by the planned point, so this peak drop isolates deck B's stop.
    runtime.stop_source_instance(&deck_b, 0)?;
    runtime.render_block(&mut output);
    let stopped_peak = peak(&output);
    let stopped_snapshot = runtime.source_timeline_snapshot();
    let stopped_source_state = stopped_snapshot
        .sources
        .iter()
        .find(|source| source.source_instance_id == deck_b)
        .ok_or(TimelineDjDemoError::MissingSource("deck B"))?
        .playback
        .state;

    Ok(TimelineDjDemoReport {
        sample_rate: config.sample_rate,
        block_frames: config.block_frames,
        source_count: initial_snapshot.sources.len(),
        overlapping_sources_playing,
        initial_peak,
        immediate_peak,
        planned_peak,
        stopped_peak,
        immediate_controls,
        planned_controls,
        stopped_source_state,
        planned_too_soon_rejected,
        planned_trigger_frame,
        planned_applied_engine_frame: planned_snapshot.engine_frame,
    })
}

fn enqueue_immediate_controls(queue: &mut crate::CommandQueue) -> Result<(), TimelineDjDemoError> {
    let deck_a = RtSourceInstanceId::new(DECK_A_ID);
    let deck_b = RtSourceInstanceId::new(DECK_B_ID);
    let commands = [
        RtCommand::SetSourceInstanceGainDb {
            source_instance_id: deck_a,
            db: -6.0,
            ramp_frames: 0,
        },
        RtCommand::SetSourceInstancePan {
            source_instance_id: deck_a,
            pan: -0.5,
            ramp_frames: 0,
        },
        RtCommand::SetSourceInstanceHighpassHz {
            source_instance_id: deck_a,
            hz: 40.0,
        },
        RtCommand::SetSourceInstanceLowpassHz {
            source_instance_id: deck_a,
            hz: 800.0,
        },
        RtCommand::SetSourceInstanceEq {
            source_instance_id: deck_a,
            low_db: 3.0,
            mid_db: -2.0,
            high_db: 1.0,
            ramp_frames: 0,
        },
        RtCommand::SetSourceInstanceReverbSendDb {
            source_instance_id: deck_a,
            send_db: -12.0,
            ramp_frames: 0,
        },
        RtCommand::SetSourceInstancePlaybackRate {
            source_instance_id: deck_a,
            rate: 1.25,
            ramp_frames: 0,
        },
        RtCommand::SetSourceInstanceReverse {
            source_instance_id: deck_a,
            reverse: true,
        },
        RtCommand::SetSourceInstancePan {
            source_instance_id: deck_b,
            pan: 0.45,
            ramp_frames: 0,
        },
    ];

    for command in commands {
        queue.enqueue(command)?;
    }

    Ok(())
}

fn file_request(
    source_instance_id: &SourceInstanceId,
    uri: &str,
    bytes: Vec<u8>,
    start_offset_ms: u64,
) -> FileSourceInstanceRequest {
    FileSourceInstanceRequest {
        source_instance_id: source_instance_id.clone(),
        uri: uri.to_string(),
        bytes,
        start_offset_ms,
        gain_db: 0.0,
        pan: 0.0,
        highpass_hz: 20.0,
        lowpass_hz: 20_000.0,
    }
}

fn effects_for(
    snapshot: &omm_protocol::SourceTimelineSnapshot,
    source_instance_id: &SourceInstanceId,
    label: &'static str,
) -> Result<DemoSourceEffects, TimelineDjDemoError> {
    let source = snapshot
        .sources
        .iter()
        .find(|source| &source.source_instance_id == source_instance_id)
        .ok_or(TimelineDjDemoError::MissingSource(label))?;
    Ok(DemoSourceEffects {
        gain_db: source.effects.gain_db,
        pan: source.effects.pan,
        highpass_hz: source.effects.highpass_hz,
        lowpass_hz: source.effects.lowpass_hz,
        eq_low_db: source.effects.eq.low_gain_db,
        eq_mid_db: source.effects.eq.mid_gain_db,
        eq_high_db: source.effects.eq.high_gain_db,
        reverb_send_db: source.effects.reverb_send_db,
        playback_rate: source.effects.playback_rate,
        reverse: source.effects.reverse,
    })
}

fn peak(frames: &[StereoFrame]) -> f32 {
    frames.iter().fold(0.0_f32, |current, frame| {
        current.max(frame.left.abs()).max(frame.right.abs())
    })
}

fn stereo_sine_wav(sample_rate: u32, duration_ms: u64, freq_hz: f32, amp: f32) -> Vec<u8> {
    let frames = frames_for_duration_ms(duration_ms, sample_rate).max(1) as usize;
    let mut samples = Vec::with_capacity(frames * 2);
    for frame_index in 0..frames {
        let phase = TAU * freq_hz * frame_index as f32 / sample_rate as f32;
        let sample = (phase.sin() * amp * i16::MAX as f32) as i16;
        samples.push(sample);
        samples.push(sample);
    }
    wav_bytes(sample_rate, 2, &samples)
}

fn wav_bytes(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let bytes_per_sample = 2_u16;
    let data_len = (samples.len() * bytes_per_sample as usize) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(
        &(sample_rate * channels as u32 * bytes_per_sample as u32).to_le_bytes(),
    );
    bytes.extend_from_slice(&(channels * bytes_per_sample).to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());

    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_dj_demo_proves_v1_acceptance() {
        let report = run_timeline_dj_demo(TimelineDjDemoConfig {
            sample_rate: 1_000,
            block_frames: 128,
            source_duration_ms: 31_000,
            planned_delay_ms: DEFAULT_PLANNED_DELAY_MS,
        })
        .expect("timeline DJ demo should run offline");

        assert_eq!(report.source_count, 2);
        assert_eq!(report.overlapping_sources_playing, 2);
        assert!(report.initial_peak > 0.05);
        assert!(report.immediate_peak > 0.01);
        assert!(report.planned_peak > 0.0);
        assert!(report.stopped_peak < report.planned_peak * 0.5);
        assert!(report.planned_too_soon_rejected);
        assert!(report.planned_applied_engine_frame >= report.planned_trigger_frame);

        assert_eq!(report.immediate_controls.gain_db, -6.0);
        assert_eq!(report.immediate_controls.pan, -0.5);
        assert_eq!(report.immediate_controls.highpass_hz, 40.0);
        assert_eq!(report.immediate_controls.lowpass_hz, 800.0);
        assert_eq!(report.immediate_controls.eq_low_db, 3.0);
        assert_eq!(report.immediate_controls.eq_mid_db, -2.0);
        assert_eq!(report.immediate_controls.eq_high_db, 1.0);
        assert_eq!(report.immediate_controls.reverb_send_db, -12.0);
        assert_eq!(report.immediate_controls.playback_rate, 1.25);
        assert!(report.immediate_controls.reverse);

        assert_eq!(report.planned_controls.gain_db, -18.0);
        assert_eq!(report.planned_controls.pan, 0.45);
        assert_eq!(report.planned_controls.eq_low_db, -3.0);
        assert_eq!(report.planned_controls.eq_mid_db, 2.0);
        assert_eq!(report.planned_controls.eq_high_db, 6.0);
        assert_eq!(report.stopped_source_state, PlaybackState::Stopped);
    }
}
