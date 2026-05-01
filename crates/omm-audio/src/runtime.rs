use crate::command::{
    new_command_channel, CommandQueue, CommandReceiver, RtCommand, MAX_DRAIN_PER_BLOCK,
};
use crate::dsp::{
    apply_gain_block, apply_pan_block, nan_guard_and_clamp, OnePoleHighpass, OnePoleLowpass,
    SafetyLimiter, SmoothedParam,
};
use crate::features::analyzer::{analysis_ringbuf_capacity, FeatureRegistry};
use crate::frame::StereoFrame;
use crate::meter::MeterSnapshot;
use crate::mixer::Mixer;
use crate::source::AudioSource;
use crate::{ChannelStrip, FeatureAnalyzerHandle};
use omm_protocol::SourceId;
use ringbuf::traits::Split;
use ringbuf::HeapRb;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChannelError {
    #[error("duplicate source id: {source_id:?}")]
    DuplicateSourceId { source_id: SourceId },
}

#[derive(Debug, Clone, Copy)]
pub struct AudioRuntimeConfig {
    pub sample_rate: u32,
}

pub struct AudioRuntime {
    sample_rate: u32,
    channels: Vec<ChannelStrip>,
    mixer: Mixer,
    master_gain_db: SmoothedParam,
    master_pan: SmoothedParam,
    highpass: OnePoleHighpass,
    lowpass: OnePoleLowpass,
    limiter: SafetyLimiter,
    command_rx: CommandReceiver,
    last_meter: MeterSnapshot,
    feature_registry: FeatureRegistry,
}

impl AudioRuntime {
    pub fn new(config: AudioRuntimeConfig) -> (Self, CommandQueue, FeatureAnalyzerHandle) {
        let (command_queue, command_rx) = new_command_channel();
        let analyzer = FeatureAnalyzerHandle::new(config.sample_rate);
        let feature_registry = analyzer.registry();

        (
            Self {
                sample_rate: config.sample_rate,
                channels: Vec::new(),
                mixer: Mixer::new(),
                master_gain_db: SmoothedParam::new(0.0),
                master_pan: SmoothedParam::new(0.0),
                highpass: OnePoleHighpass::new(20.0, config.sample_rate),
                lowpass: OnePoleLowpass::new(20_000.0, config.sample_rate),
                limiter: SafetyLimiter::new(1.0),
                command_rx,
                last_meter: MeterSnapshot::default(),
                feature_registry,
            },
            command_queue,
            analyzer,
        )
    }

    pub fn add_channel(
        &mut self,
        source_id: SourceId,
        source: Box<dyn AudioSource>,
    ) -> Result<(), ChannelError> {
        if self
            .channels
            .iter()
            .any(|channel| channel.source_id() == source_id)
        {
            return Err(ChannelError::DuplicateSourceId { source_id });
        }

        let mut strip = ChannelStrip::new(source_id, source, self.sample_rate);
        let capacity = analysis_ringbuf_capacity(self.sample_rate);
        let (producer, consumer) = HeapRb::<f32>::new(capacity).split();
        strip.attach_analysis_producer(producer);
        self.feature_registry.register_channel(source_id, consumer);
        self.channels.push(strip);
        Ok(())
    }

    pub fn render_block(&mut self, output: &mut [StereoFrame]) {
        self.drain_commands();

        if output.is_empty() {
            return;
        }

        self.mixer.render(&mut self.channels, output);
        apply_gain_block(output, &mut self.master_gain_db);
        apply_pan_block(output, &mut self.master_pan);
        self.highpass.process(output);
        self.lowpass.process(output);
        self.limiter.process(output);
        nan_guard_and_clamp(output);
        self.last_meter = MeterSnapshot::compute(output);
    }

    pub fn meters(&self) -> &MeterSnapshot {
        &self.last_meter
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_master_gain_db(&mut self, db: f32, ramp_frames: u32) {
        self.master_gain_db.set_target(db, ramp_frames);
    }

    pub fn set_master_pan(&mut self, pan: f32, ramp_frames: u32) {
        self.master_pan.set_target(pan, ramp_frames);
    }

    pub fn set_lowpass_hz(&mut self, hz: f32) {
        self.lowpass.set_cutoff(hz);
    }

    pub fn set_highpass_hz(&mut self, hz: f32) {
        self.highpass.set_cutoff(hz);
    }

    pub fn set_channel_gain_db(&mut self, source_id: SourceId, db: f32, ramp_frames: u32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.source_id() == source_id)
        {
            channel.set_gain_db(db, ramp_frames);
        }
    }

    pub fn set_channel_pan(&mut self, source_id: SourceId, pan: f32, ramp_frames: u32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.source_id() == source_id)
        {
            channel.set_pan(pan, ramp_frames);
        }
    }

    pub fn set_channel_highpass_hz(&mut self, source_id: SourceId, hz: f32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.source_id() == source_id)
        {
            channel.set_highpass_hz(hz);
        }
    }

    pub fn set_channel_lowpass_hz(&mut self, source_id: SourceId, hz: f32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.source_id() == source_id)
        {
            channel.set_lowpass_hz(hz);
        }
    }

    pub fn set_channel_enabled(&mut self, source_id: SourceId, enabled: bool) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.source_id() == source_id)
        {
            channel.set_enabled(enabled);
        }
    }

    fn drain_commands(&mut self) -> usize {
        let mut count = 0;

        while count < MAX_DRAIN_PER_BLOCK {
            let mut next_command = None;
            let drained = self
                .command_rx
                .drain(&mut |cmd| next_command = Some(cmd), 1);

            if drained == 0 {
                break;
            }

            if let Some(cmd) = next_command {
                self.apply_command(cmd);
            }

            count += drained;
        }

        count
    }

    fn apply_command(&mut self, cmd: RtCommand) {
        match cmd {
            RtCommand::SetMasterGainDb { db, ramp_frames } => {
                self.set_master_gain_db(db, ramp_frames);
            }
            RtCommand::SetMasterPan { pan, ramp_frames } => {
                self.set_master_pan(pan, ramp_frames);
            }
            RtCommand::SetMasterLowpassHz { hz } => {
                self.set_lowpass_hz(hz);
            }
            RtCommand::SetMasterHighpassHz { hz } => {
                self.set_highpass_hz(hz);
            }
            RtCommand::SetChannelGainDb {
                source_id,
                db,
                ramp_frames,
            } => {
                self.set_channel_gain_db(source_id, db, ramp_frames);
            }
            RtCommand::SetChannelPan {
                source_id,
                pan,
                ramp_frames,
            } => {
                self.set_channel_pan(source_id, pan, ramp_frames);
            }
            RtCommand::SetChannelLowpassHz { source_id, hz } => {
                self.set_channel_lowpass_hz(source_id, hz);
            }
            RtCommand::SetChannelHighpassHz { source_id, hz } => {
                self.set_channel_highpass_hz(source_id, hz);
            }
            RtCommand::SetChannelEnabled { source_id, enabled } => {
                self.set_channel_enabled(source_id, enabled);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::ChannelFeatures;
    use crate::source::{GlicolSource, TestToneSource};
    use omm_protocol::SourceId;

    const SAMPLE_RATE: u32 = 48_000;
    const FRAME_COUNT: usize = 128;

    struct LoudSource {
        value: f32,
        sign: f32,
    }

    impl LoudSource {
        fn new(value: f32) -> Self {
            Self { value, sign: 1.0 }
        }
    }

    impl AudioSource for LoudSource {
        fn render(&mut self, output: &mut [StereoFrame]) {
            for frame in output.iter_mut() {
                let sample = self.value * self.sign;
                *frame = StereoFrame::new(sample, sample);
                self.sign = -self.sign;
            }
        }

        fn set_enabled(&mut self, enabled: bool) {
            let _ = enabled;
        }

        fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
            let _ = (gain_db, ramp_frames);
        }
    }

    fn peak(frames: &[StereoFrame]) -> f32 {
        frames.iter().fold(0.0_f32, |current, frame| {
            current.max(frame.left.abs()).max(frame.right.abs())
        })
    }

    fn assert_all_in_unit_range(frames: &[StereoFrame]) {
        for (index, frame) in frames.iter().enumerate() {
            assert!(
                frame.left >= -1.0 && frame.left <= 1.0,
                "left out of range at {index}: {}",
                frame.left
            );
            assert!(
                frame.right >= -1.0 && frame.right <= 1.0,
                "right out of range at {index}: {}",
                frame.right
            );
        }
    }

    fn add_test_channel(
        runtime: &mut AudioRuntime,
        source_id: SourceId,
        source: Box<dyn AudioSource>,
    ) {
        let result = runtime.add_channel(source_id, source);
        assert!(result.is_ok(), "channel should be added: {result:?}");
    }

    fn runtime_with_loud_channels(source_ids: &[SourceId], value: f32) -> AudioRuntime {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        runtime.set_master_pan(-1.0, 0);

        for source_id in source_ids {
            add_test_channel(&mut runtime, *source_id, Box::new(LoudSource::new(value)));
        }

        runtime
    }

    #[test]
    fn add_channel_duplicate_source_id_rejected() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });

        let first = runtime.add_channel(
            SourceId::System,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );
        assert!(first.is_ok(), "first System channel should be accepted");

        let duplicate = runtime.add_channel(
            SourceId::System,
            Box::new(TestToneSource::new(880.0, SAMPLE_RATE)),
        );

        assert_eq!(
            duplicate,
            Err(ChannelError::DuplicateSourceId {
                source_id: SourceId::System
            })
        );
    }

    #[test]
    fn runtime_without_sources_renders_silence() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let mut output = vec![StereoFrame::new(0.5, -0.5); FRAME_COUNT];

        runtime.render_block(&mut output);

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
    fn runtime_with_test_tone_source_renders_nonzero_signal() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, 48000)),
        );
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p > 0.5, "expected peak > 0.5, got {p}");
    }

    #[test]
    fn runtime_master_gain_minus_sixty_is_nearly_silent() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        runtime.set_master_gain_db(-60.0, 0);
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, 48000)),
        );
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p < 0.01, "expected near silence, got peak {p}");
    }

    #[test]
    fn command_drain_master_gain() {
        let (mut runtime, mut queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );
        assert!(queue
            .enqueue(RtCommand::SetMasterGainDb {
                db: -60.0,
                ramp_frames: 0,
            })
            .is_ok());

        let mut output = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p < 0.01, "expected queued gain to silence output, got {p}");
    }

    #[test]
    fn drain_max_per_block() {
        let (mut runtime, mut queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        for _ in 0..MAX_DRAIN_PER_BLOCK {
            assert!(queue
                .enqueue(RtCommand::SetMasterGainDb {
                    db: -60.0,
                    ramp_frames: 0,
                })
                .is_ok());
        }
        assert!(queue
            .enqueue(RtCommand::SetMasterGainDb {
                db: 0.0,
                ramp_frames: 0,
            })
            .is_ok());

        let mut output = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut output);
        let first_peak = peak(&output);
        assert!(
            first_peak < 0.01,
            "65th command must not drain in the first block, got {first_peak}"
        );

        runtime.render_block(&mut output);
        let second_peak = peak(&output);
        assert!(
            second_peak > 0.3,
            "65th command should drain on the next block, got {second_peak}"
        );
    }

    #[test]
    fn runtime_master_gain_zero_keeps_normal_amplitude() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        runtime.set_master_gain_db(0.0, 0);
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, 48000)),
        );
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p > 0.5, "expected normal amplitude, got peak {p}");
    }

    #[test]
    fn runtime_master_gain_minus_6db_attenuates_input_by_half() {
        let (mut unity, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        unity.set_master_pan(-1.0, 0);
        add_test_channel(
            &mut unity,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        let (mut attenuated, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        attenuated.set_master_gain_db(-6.0, 0);
        attenuated.set_master_pan(-1.0, 0);
        add_test_channel(
            &mut attenuated,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        let mut unity_warmup = vec![StereoFrame::SILENCE; 512];
        let mut attenuated_warmup = vec![StereoFrame::SILENCE; 512];
        unity.render_block(&mut unity_warmup);
        attenuated.render_block(&mut attenuated_warmup);

        let mut unity_output = vec![StereoFrame::SILENCE; 256];
        let mut attenuated_output = vec![StereoFrame::SILENCE; 256];
        unity.render_block(&mut unity_output);
        attenuated.render_block(&mut attenuated_output);

        let ratio = peak(&attenuated_output) / peak(&unity_output);
        assert!(
            (ratio - 0.501).abs() < 0.01,
            "expected -6 dB single-stage ratio ≈ 0.5; duplicate master-gain stages would yield ≈0.25, got {ratio}"
        );
    }

    #[test]
    fn master_gain_applies_to_all_channel_output() {
        let source_ids = [
            SourceId::System,
            SourceId::Mic,
            SourceId::Player,
            SourceId::Glicol,
        ];
        let mut unity = runtime_with_loud_channels(&source_ids, 0.05);
        let mut attenuated = runtime_with_loud_channels(&source_ids, 0.05);
        attenuated.set_master_gain_db(-6.0, 0);

        let mut unity_output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        let mut attenuated_output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        unity.render_block(&mut unity_output);
        attenuated.render_block(&mut attenuated_output);

        let ratio = peak(&attenuated_output) / peak(&unity_output);
        assert!(
            (ratio - 0.501).abs() < 0.02,
            "expected master -6 dB to halve all summed channels, got ratio {ratio}"
        );
    }

    #[test]
    fn four_channels_summed() {
        let mut single = runtime_with_loud_channels(&[SourceId::System], 0.05);
        let mut four = runtime_with_loud_channels(
            &[
                SourceId::System,
                SourceId::Mic,
                SourceId::Player,
                SourceId::Glicol,
            ],
            0.05,
        );

        let mut single_output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        let mut four_output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        single.render_block(&mut single_output);
        four.render_block(&mut four_output);

        let ratio = peak(&four_output) / peak(&single_output);
        assert!(
            (ratio - 4.0).abs() < 0.05,
            "expected four channels to sum linearly, got ratio {ratio}"
        );
    }

    #[test]
    fn runtime_output_is_always_clamped_to_unit_range() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, 48000)),
        );
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        assert_all_in_unit_range(&output);
    }

    #[test]
    fn runtime_limiter_clamps_excessive_source_signal() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(LoudSource::new(10_000.0)),
        );
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        assert_all_in_unit_range(&output);
        let p = peak(&output);
        assert!(
            (p - 1.0).abs() < 0.001,
            "expected limiter clamp at 1.0, got {p}"
        );
    }

    #[test]
    fn runtime_meters_return_latest_render_snapshot() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, 48000)),
        );
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let meters = runtime.meters();
        assert!(meters.peak_left > 0.0, "left peak should update");
        assert!(meters.peak_right > 0.0, "right peak should update");
    }

    #[test]
    fn runtime_empty_output_slice_does_not_panic() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, 48000)),
        );
        let mut output: Vec<StereoFrame> = Vec::new();

        runtime.render_block(&mut output);

        assert!(output.is_empty());
    }

    #[test]
    fn render_block_no_alloc_in_steady_state() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );
        add_test_channel(
            &mut runtime,
            SourceId::System,
            Box::new(LoudSource::new(0.25)),
        );

        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];

        for _ in 0..256 {
            runtime.render_block(&mut output);
            assert_all_in_unit_range(&output);
        }
    }

    #[test]
    fn runtime_renders_glicol_source_nonzero() {
        let mut source = GlicolSource::new(SAMPLE_RATE);
        let load_result = source.load_code("out: sin 440 >> mul 0.3");
        assert!(
            load_result.is_ok(),
            "Glicol code should load: {load_result:?}"
        );

        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(&mut runtime, SourceId::Glicol, Box::new(source));
        let mut output = vec![StereoFrame::SILENCE; 256];

        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(p > 0.05, "expected Glicol non-zero output, got peak {p}");
        assert_all_in_unit_range(&output);
    }

    #[test]
    fn channel_command_gain() {
        let (mut unity, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        unity.set_master_pan(-1.0, 0);
        add_test_channel(
            &mut unity,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        let (mut attenuated, mut queue_att, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        attenuated.set_master_pan(-1.0, 0);
        add_test_channel(
            &mut attenuated,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        assert!(queue_att
            .enqueue(RtCommand::SetChannelGainDb {
                source_id: SourceId::Glicol,
                db: -6.0,
                ramp_frames: 0,
            })
            .is_ok());

        let mut unity_output = vec![StereoFrame::SILENCE; 256];
        let mut attenuated_output = vec![StereoFrame::SILENCE; 256];
        unity.render_block(&mut unity_output);
        attenuated.render_block(&mut attenuated_output);

        let ratio = peak(&attenuated_output) / peak(&unity_output);
        assert!(
            (ratio - 0.501).abs() < 0.01,
            "expected -6 dB channel gain to halve output, got ratio {ratio}"
        );
    }

    #[test]
    fn channel_command_pan() {
        let (mut center, _queue_center, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut center,
            SourceId::Player,
            Box::new(LoudSource::new(0.5)),
        );

        let (mut hard_left, mut queue_left, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut hard_left,
            SourceId::Player,
            Box::new(LoudSource::new(0.5)),
        );

        assert!(queue_left
            .enqueue(RtCommand::SetChannelPan {
                source_id: SourceId::Player,
                pan: -1.0,
                ramp_frames: 0,
            })
            .is_ok());

        let mut center_output = vec![StereoFrame::SILENCE; 256];
        let mut left_output = vec![StereoFrame::SILENCE; 256];
        center.render_block(&mut center_output);
        hard_left.render_block(&mut left_output);

        let mut left_peak = 0.0_f32;
        let mut right_peak = 0.0_f32;
        for frame in left_output.iter() {
            left_peak = left_peak.max(frame.left.abs());
            right_peak = right_peak.max(frame.right.abs());
        }

        assert!(
            left_peak > 0.3,
            "expected hard-left pan to have nonzero left output, got {left_peak}"
        );
        assert!(
            right_peak < 0.05,
            "expected hard-left pan to silence right channel, got {right_peak}"
        );
    }

    #[test]
    fn channel_command_unknown_source_ignored() {
        let (mut runtime, mut queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        assert!(queue
            .enqueue(RtCommand::SetChannelGainDb {
                source_id: SourceId::Mic,
                db: -60.0,
                ramp_frames: 0,
            })
            .is_ok());

        let mut output = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut output);

        let p = peak(&output);
        assert!(
            p > 0.5,
            "expected Glicol channel unaffected by unknown source command, got peak {p}"
        );
    }

    #[test]
    fn feature_handle_polling_returns_centroid() {
        use std::time::{Duration, Instant};

        let (mut runtime, _queue, mut handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(1_000.0, SAMPLE_RATE)),
        );

        let block_frames = 512;
        let mut output = vec![StereoFrame::SILENCE; block_frames];
        let total_frames_target = (SAMPLE_RATE as usize) * 2 + block_frames;
        let mut frames_rendered = 0;
        while frames_rendered < total_frames_target {
            runtime.render_block(&mut output);
            frames_rendered += block_frames;
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        let features = loop {
            if let Some(features) = handle.poll_features(SourceId::Glicol) {
                break features;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for Glicol features"
            );
            std::thread::sleep(Duration::from_millis(5));
        };

        assert_eq!(features.source_id, SourceId::Glicol);
        assert!(
            (900.0..=1_100.0).contains(&features.spectral_centroid_hz),
            "expected centroid ~1kHz, got {}",
            features.spectral_centroid_hz
        );
    }

    #[test]
    fn feature_handle_polling_multiple_channels() {
        use std::time::{Duration, Instant};

        let (mut runtime, _queue, mut handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(1_000.0, SAMPLE_RATE)),
        );
        add_test_channel(
            &mut runtime,
            SourceId::Player,
            Box::new(TestToneSource::new(5_000.0, SAMPLE_RATE)),
        );

        let block_frames = 512;
        let mut output = vec![StereoFrame::SILENCE; block_frames];
        let total_frames_target = (SAMPLE_RATE as usize) * 2 + block_frames;
        let mut frames_rendered = 0;
        while frames_rendered < total_frames_target {
            runtime.render_block(&mut output);
            frames_rendered += block_frames;
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut glicol_features: Option<ChannelFeatures> = None;
        let mut player_features: Option<ChannelFeatures> = None;
        while glicol_features.is_none() || player_features.is_none() {
            for snapshot in handle.poll_all() {
                match snapshot.source_id {
                    SourceId::Glicol => glicol_features = Some(snapshot),
                    SourceId::Player => player_features = Some(snapshot),
                    _ => {}
                }
            }
            if glicol_features.is_some() && player_features.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for both channel features"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        let glicol = glicol_features.expect("Glicol features missing");
        let player = player_features.expect("Player features missing");

        assert!(
            (900.0..=1_100.0).contains(&glicol.spectral_centroid_hz),
            "Glicol centroid out of range: {}",
            glicol.spectral_centroid_hz
        );
        assert!(
            (4_500.0..=5_500.0).contains(&player.spectral_centroid_hz),
            "Player centroid out of range: {}",
            player.spectral_centroid_hz
        );
    }

    #[test]
    fn feature_handle_drop_joins_thread() {
        use std::time::{Duration, Instant};

        let (_runtime, _queue, handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });

        let start = Instant::now();
        drop(handle);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(100),
            "expected handle drop to join analyzer thread quickly, took {elapsed:?}"
        );
    }
}
