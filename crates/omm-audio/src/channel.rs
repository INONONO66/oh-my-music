use crate::constants::MAX_BLOCK_FRAMES;
use crate::dsp::{
    apply_gain_block, apply_pan_block, OnePoleHighpass, OnePoleLowpass, SimpleReverb,
    SmoothedParam, ThreeBandEq,
};
use crate::frame::StereoFrame;
use crate::source::AudioSource;
use omm_protocol::{
    PlaybackState, PlaybackStatusAuthority, SourceAssetRef, SourceEffectStatus, SourceId,
    SourceInstanceId, SourceKind, SourcePlaybackStatus, SourceTimelinePlacement,
    TimelineActiveWindow, TimelineSourceInstance,
};
use ringbuf::traits::Producer;
use ringbuf::HeapProd;

pub struct ChannelStrip {
    legacy_source_id: Option<SourceId>,
    source_instance_id: SourceInstanceId,
    source_kind: SourceKind,
    asset_ref: Option<SourceAssetRef>,
    timeline: SourceTimelinePlacement,
    playback_authority: PlaybackStatusAuthority,
    playback_state: PlaybackState,
    source: Box<dyn AudioSource>,
    gain_db: SmoothedParam,
    pan: SmoothedParam,
    eq: ThreeBandEq,
    highpass: OnePoleHighpass,
    lowpass: OnePoleLowpass,
    reverb: SimpleReverb,
    scratch: Vec<StereoFrame>,
    enabled: bool,
    pending_stop_after_frames: Option<usize>,
    analysis_producer: Option<HeapProd<f32>>,
    analysis_scratch: Vec<f32>,
}

struct ChannelStripMetadata {
    legacy_source_id: Option<SourceId>,
    source_instance_id: SourceInstanceId,
    source_kind: SourceKind,
    asset_ref: Option<SourceAssetRef>,
    timeline: SourceTimelinePlacement,
    playback_authority: PlaybackStatusAuthority,
}

impl ChannelStrip {
    pub fn new(source_id: SourceId, source: Box<dyn AudioSource>, sample_rate: u32) -> Self {
        Self::new_with_metadata(
            ChannelStripMetadata {
                legacy_source_id: Some(source_id),
                source_instance_id: SourceInstanceId::legacy(source_id),
                source_kind: SourceKind::from_legacy_source(source_id),
                asset_ref: legacy_asset_ref(source_id),
                timeline: SourceTimelinePlacement::legacy_always_on(),
                playback_authority: PlaybackStatusAuthority::LegacyChannelEnabled,
            },
            source,
            sample_rate,
        )
    }

    pub fn new_timeline_source(
        source_instance_id: SourceInstanceId,
        source_kind: SourceKind,
        asset_ref: Option<SourceAssetRef>,
        timeline: SourceTimelinePlacement,
        source: Box<dyn AudioSource>,
        sample_rate: u32,
    ) -> Self {
        Self::new_with_metadata(
            ChannelStripMetadata {
                legacy_source_id: None,
                source_instance_id,
                source_kind,
                asset_ref,
                timeline,
                playback_authority: PlaybackStatusAuthority::TimelineTransport,
            },
            source,
            sample_rate,
        )
    }

    fn new_with_metadata(
        metadata: ChannelStripMetadata,
        source: Box<dyn AudioSource>,
        sample_rate: u32,
    ) -> Self {
        Self {
            legacy_source_id: metadata.legacy_source_id,
            source_instance_id: metadata.source_instance_id,
            source_kind: metadata.source_kind,
            asset_ref: metadata.asset_ref,
            timeline: metadata.timeline,
            playback_authority: metadata.playback_authority,
            playback_state: PlaybackState::Playing,
            source,
            gain_db: SmoothedParam::new(0.0),
            pan: SmoothedParam::new(0.0),
            eq: ThreeBandEq::new(sample_rate),
            highpass: OnePoleHighpass::new(20.0, sample_rate),
            lowpass: OnePoleLowpass::new(20_000.0, sample_rate),
            reverb: SimpleReverb::new(sample_rate),
            scratch: Vec::with_capacity(MAX_BLOCK_FRAMES),
            enabled: true,
            pending_stop_after_frames: None,
            analysis_producer: None,
            analysis_scratch: Vec::with_capacity(MAX_BLOCK_FRAMES),
        }
    }

    pub fn legacy_source_id(&self) -> Option<SourceId> {
        self.legacy_source_id
    }

    pub fn source_instance_id(&self) -> &SourceInstanceId {
        &self.source_instance_id
    }

    pub fn timeline_source_snapshot(
        &self,
        engine_frame: u64,
        sample_rate: u32,
    ) -> TimelineSourceInstance {
        let playback = self.playback_status(engine_frame, sample_rate);

        TimelineSourceInstance {
            source_instance_id: self.source_instance_id.clone(),
            source_kind: self.source_kind,
            asset_ref: self.asset_ref.clone(),
            timeline: self.timeline.clone(),
            playback,
            effects: self.effect_status(),
            legacy_bridge: self
                .legacy_source_id
                .map(|source_id| omm_protocol::LegacySourceBridge { source_id }),
        }
    }

    fn playback_status(&self, engine_frame: u64, sample_rate: u32) -> SourcePlaybackStatus {
        if self.playback_authority == PlaybackStatusAuthority::LegacyChannelEnabled {
            return SourcePlaybackStatus::legacy_enabled(self.enabled);
        }

        SourcePlaybackStatus {
            state: self.playback_state,
            authority: self.playback_authority,
            timeline_position_ms: Some(frames_to_ms(engine_frame, sample_rate)),
            source_position_ms: self
                .source
                .position_frames()
                .map(|frames| frames_to_ms(frames, sample_rate)),
            loop_enabled: false,
        }
    }

    fn effect_status(&self) -> SourceEffectStatus {
        SourceEffectStatus {
            gain_db: self.gain_db.current(),
            pan: self.pan.current(),
            highpass_hz: self.highpass.cutoff_hz(),
            lowpass_hz: self.lowpass.cutoff_hz(),
            eq: self.eq.status(),
            reverb_send_db: self.reverb.send_db(),
            playback_rate: self.source.playback_rate().unwrap_or(1.0),
            reverse: self.source.reverse().unwrap_or(false),
        }
    }

    pub fn render(&mut self, output: &mut [StereoFrame]) {
        debug_assert!(output.len() <= MAX_BLOCK_FRAMES);

        if output.is_empty() {
            return;
        }

        if !self.enabled {
            output.fill(StereoFrame::SILENCE);
            return;
        }

        let n = output.len();
        self.scratch.resize(n, StereoFrame::SILENCE);

        self.source.render(&mut self.scratch[..n]);
        apply_gain_block(&mut self.scratch[..n], &mut self.gain_db);
        apply_pan_block(&mut self.scratch[..n], &mut self.pan);
        self.eq.process(&mut self.scratch[..n]);
        self.highpass.process(&mut self.scratch[..n]);
        self.lowpass.process(&mut self.scratch[..n]);
        self.reverb.process(&mut self.scratch[..n]);
        output.copy_from_slice(&self.scratch[..n]);

        if self.source.is_finished() {
            self.playback_state = PlaybackState::Ended;
        }

        if let Some(frames_left) = self.pending_stop_after_frames {
            if n >= frames_left {
                self.enabled = false;
                self.source.set_enabled(false);
                self.pending_stop_after_frames = None;
                self.playback_state = PlaybackState::Stopped;
            } else {
                self.pending_stop_after_frames = Some(frames_left - n);
            }
        }

        if let Some(producer) = &mut self.analysis_producer {
            self.analysis_scratch.resize(n, 0.0);
            for (i, frame) in output.iter().enumerate() {
                self.analysis_scratch[i] = (frame.left + frame.right) * 0.5;
            }
            for sample in &self.analysis_scratch[..n] {
                let _ = producer.try_push(*sample);
            }
        }
    }

    pub fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
        self.gain_db.set_target(gain_db, ramp_frames);
    }

    pub fn set_pan(&mut self, pan: f32, ramp_frames: u32) {
        self.pan.set_target(pan, ramp_frames);
    }

    pub fn set_eq_gains_db(&mut self, low_db: f32, mid_db: f32, high_db: f32, ramp_frames: u32) {
        self.eq.set_gains_db(low_db, mid_db, high_db, ramp_frames);
    }

    pub fn set_eq_low_gain_db(&mut self, low_db: f32, ramp_frames: u32) {
        self.eq.set_low_gain_db(low_db, ramp_frames);
    }

    pub fn set_eq_mid_gain_db(&mut self, mid_db: f32, ramp_frames: u32) {
        self.eq.set_mid_gain_db(mid_db, ramp_frames);
    }

    pub fn set_eq_high_gain_db(&mut self, high_db: f32, ramp_frames: u32) {
        self.eq.set_high_gain_db(high_db, ramp_frames);
    }

    pub fn eq_target_status(&self) -> omm_protocol::SourceEqStatus {
        self.eq.target_status()
    }

    pub fn set_highpass_hz(&mut self, hz: f32) {
        self.highpass.set_cutoff(hz);
    }

    pub fn set_lowpass_hz(&mut self, hz: f32) {
        self.lowpass.set_cutoff(hz);
    }

    pub fn set_reverb_send_db(&mut self, send_db: f32, ramp_frames: u32) {
        self.reverb.set_send_db(send_db, ramp_frames);
    }

    pub fn set_playback_rate(&mut self, rate: f32, ramp_frames: u32) -> bool {
        self.source.set_playback_rate(rate, ramp_frames)
    }

    pub fn set_reverse(&mut self, reverse: bool) -> bool {
        self.source.set_reverse(reverse)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.source.set_enabled(enabled);
        self.pending_stop_after_frames = None;
        self.playback_state = if enabled {
            PlaybackState::Playing
        } else {
            PlaybackState::Stopped
        };
    }

    pub fn stop(&mut self, fade_frames: u32) {
        if fade_frames == 0 {
            self.set_enabled(false);
            return;
        }

        self.gain_db.set_target(-60.0, fade_frames);
        self.pending_stop_after_frames = Some(fade_frames as usize);
    }

    pub fn attach_analysis_producer(&mut self, producer: HeapProd<f32>) {
        self.analysis_producer = Some(producer);
    }
}

fn frames_to_ms(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    ((frames as u128 * 1_000) / sample_rate as u128) as u64
}

fn legacy_asset_ref(source_id: SourceId) -> Option<SourceAssetRef> {
    match source_id {
        SourceId::System => Some(SourceAssetRef::LiveInput {
            label: "system".to_string(),
        }),
        SourceId::Mic => Some(SourceAssetRef::LiveInput {
            label: "mic".to_string(),
        }),
        SourceId::Player => None,
        SourceId::Glicol => Some(SourceAssetRef::Generated {
            engine: omm_protocol::GeneratedEngine::Glicol,
            code_ref: None,
        }),
    }
}

pub(crate) fn file_timeline(start_ms: u64, source_start_offset_ms: u64) -> SourceTimelinePlacement {
    SourceTimelinePlacement {
        active_windows: vec![TimelineActiveWindow {
            timeline_start_ms: start_ms,
            timeline_end_ms: None,
            source_start_offset_ms,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::ENGINE_SAMPLE_RATE;
    use std::f32::consts::TAU;

    const FRAME_COUNT: usize = 512;
    const EPSILON: f32 = 0.03;

    struct SineSource {
        phase: f32,
        phase_increment: f32,
        enabled: bool,
    }

    impl SineSource {
        fn new(freq_hz: f32) -> Self {
            Self {
                phase: 0.0,
                phase_increment: TAU * freq_hz / ENGINE_SAMPLE_RATE as f32,
                enabled: true,
            }
        }
    }

    impl AudioSource for SineSource {
        fn render(&mut self, output: &mut [StereoFrame]) {
            if !self.enabled {
                output.fill(StereoFrame::SILENCE);
                return;
            }

            for frame in output.iter_mut() {
                let sample = self.phase.sin();
                *frame = StereoFrame::new(sample, sample);
                self.phase += self.phase_increment;
                if self.phase >= TAU {
                    self.phase -= TAU;
                }
            }
        }

        fn set_enabled(&mut self, enabled: bool) {
            self.enabled = enabled;
        }

        fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
            let _ = (gain_db, ramp_frames);
        }
    }

    fn render_sine(freq_hz: f32) -> ChannelStrip {
        ChannelStrip::new_timeline_source(
            SourceInstanceId::new("glicol:main"),
            SourceKind::Generated,
            Some(SourceAssetRef::Generated {
                engine: omm_protocol::GeneratedEngine::Glicol,
                code_ref: None,
            }),
            SourceTimelinePlacement::always_on(),
            Box::new(SineSource::new(freq_hz)),
            ENGINE_SAMPLE_RATE,
        )
    }

    fn render_after_warmup(strip: &mut ChannelStrip, warmup_blocks: usize) -> Vec<StereoFrame> {
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        for _ in 0..warmup_blocks {
            strip.render(&mut output);
        }
        strip.render(&mut output);
        output
    }

    fn peak_left(frames: &[StereoFrame]) -> f32 {
        frames
            .iter()
            .fold(0.0_f32, |current, frame| current.max(frame.left.abs()))
    }

    fn peak_right(frames: &[StereoFrame]) -> f32 {
        frames
            .iter()
            .fold(0.0_f32, |current, frame| current.max(frame.right.abs()))
    }

    fn assert_near(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {actual} to be within {tolerance} of {expected}"
        );
    }

    #[test]
    fn channel_default_pan_uses_equal_power_center_attenuation() {
        let mut center = render_sine(1_000.0);
        let mut hard_left = render_sine(1_000.0);
        hard_left.set_pan(-1.0, 0);

        let center_output = render_after_warmup(&mut center, 8);
        let hard_left_output = render_after_warmup(&mut hard_left, 8);

        let ratio = peak_left(&center_output) / peak_left(&hard_left_output);
        assert_near(ratio, std::f32::consts::FRAC_1_SQRT_2, EPSILON);
    }

    #[test]
    fn channel_minus_six_db_gain_attenuates_relative_to_unity() {
        let mut unity = render_sine(1_000.0);
        unity.set_pan(-1.0, 0);
        let mut attenuated = render_sine(1_000.0);
        attenuated.set_pan(-1.0, 0);
        attenuated.set_gain_db(-6.0, 0);

        let unity_output = render_after_warmup(&mut unity, 8);
        let attenuated_output = render_after_warmup(&mut attenuated, 8);

        let ratio = peak_left(&attenuated_output) / peak_left(&unity_output);
        assert_near(ratio, 0.501, EPSILON);
    }

    #[test]
    fn channel_hard_left_pan_removes_right_channel() {
        let mut strip = render_sine(1_000.0);
        strip.set_pan(-1.0, 0);

        let output = render_after_warmup(&mut strip, 8);

        assert!(peak_left(&output) > 0.7);
        assert!(peak_right(&output) < 0.001);
    }

    #[test]
    fn channel_lowpass_attenuates_high_frequencies() {
        let mut reference = render_sine(10_000.0);
        reference.set_pan(-1.0, 0);
        let mut filtered = render_sine(10_000.0);
        filtered.set_pan(-1.0, 0);
        filtered.set_lowpass_hz(1_000.0);

        let reference_output = render_after_warmup(&mut reference, 8);
        let filtered_output = render_after_warmup(&mut filtered, 8);

        let ratio = peak_left(&filtered_output) / peak_left(&reference_output);
        assert!(ratio < 0.35, "expected high attenuation, got {ratio}");
    }

    #[test]
    fn channel_highpass_attenuates_low_frequencies() {
        let mut reference = render_sine(50.0);
        reference.set_pan(-1.0, 0);
        let mut filtered = render_sine(50.0);
        filtered.set_pan(-1.0, 0);
        filtered.set_highpass_hz(1_000.0);

        let reference_output = render_after_warmup(&mut reference, 8);
        let filtered_output = render_after_warmup(&mut filtered, 8);

        let ratio = peak_left(&filtered_output) / peak_left(&reference_output);
        assert!(ratio < 0.35, "expected low attenuation, got {ratio}");
    }

    #[test]
    fn channel_disabled_renders_silence() {
        let mut strip = render_sine(1_000.0);
        strip.set_enabled(false);
        let mut output = vec![StereoFrame::new(0.5, -0.5); FRAME_COUNT];

        strip.render(&mut output);

        for (index, frame) in output.iter().enumerate() {
            assert_eq!(frame.left, 0.0, "left non-zero at {index}: {}", frame.left);
            assert_eq!(
                frame.right, 0.0,
                "right non-zero at {index}: {}",
                frame.right
            );
        }
    }

    #[test]
    fn channel_empty_output_is_noop() {
        let mut strip = render_sine(1_000.0);
        let mut output = Vec::new();

        strip.render(&mut output);

        assert!(output.is_empty());
    }

    #[test]
    fn analysis_mid_downmix_pushed() {
        use ringbuf::traits::{Consumer, Split};
        use ringbuf::HeapRb;

        let ringbuf = HeapRb::<f32>::new(FRAME_COUNT);
        let (producer, mut consumer) = ringbuf.split();

        let mut strip = render_sine(1_000.0);
        strip.set_pan(-1.0, 0);
        strip.attach_analysis_producer(producer);

        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        strip.render(&mut output);

        let mut pushed_samples = Vec::new();
        while let Some(sample) = consumer.try_pop() {
            pushed_samples.push(sample);
        }

        assert_eq!(
            pushed_samples.len(),
            FRAME_COUNT,
            "expected {} samples pushed, got {}",
            FRAME_COUNT,
            pushed_samples.len()
        );

        for (i, &sample) in pushed_samples.iter().enumerate() {
            let expected = (output[i].left + output[i].right) * 0.5;
            assert_near(sample, expected, 0.001);
        }
    }
}
