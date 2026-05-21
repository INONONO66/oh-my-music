use crate::channel::file_timeline;
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
use crate::source::{AudioSource, PlayerSource, PlayerSourceError};
use crate::{
    ChannelStrip, FeatureAnalyzerHandle, RtCommandScheduleRequest, RtCommandScheduler,
    RtCommandSchedulerError,
};
use omm_protocol::{
    frames_for_duration_ms, ParamId, SourceAssetRef, SourceId, SourceInstanceId, SourceKind,
    SourceTimelineSnapshot, SourceTimelineValidationError,
};
use ringbuf::traits::Split;
use ringbuf::HeapRb;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChannelError {
    #[error("duplicate source id: {source_id:?}")]
    DuplicateSourceId { source_id: SourceId },
}

#[derive(Debug)]
pub struct FileSourceInstanceRequest {
    pub source_instance_id: SourceInstanceId,
    pub uri: String,
    pub bytes: Vec<u8>,
    pub start_offset_ms: u64,
    pub gain_db: f32,
    pub pan: f32,
    pub highpass_hz: f32,
    pub lowpass_hz: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAutomationTarget {
    GainDb,
    Pan,
    EqLowGainDb,
    EqMidGainDb,
    EqHighGainDb,
    ReverbSendDb,
    PlaybackRate,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceAutomationRamp {
    pub target: SourceAutomationTarget,
    pub end_value: f32,
    pub duration_frames: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceInstanceError {
    #[error(transparent)]
    Validation(#[from] SourceTimelineValidationError),
    #[error("source instance already exists: {source_instance_id}")]
    DuplicateSourceInstance { source_instance_id: String },
    #[error("source instance id is reserved for fixed legacy channels: {source_instance_id}")]
    ReservedSourceInstanceId { source_instance_id: String },
    #[error("start offset {start_offset_ms} ms is not before file duration {duration_ms} ms")]
    StartOffsetBeyondDuration {
        start_offset_ms: u64,
        duration_ms: u64,
    },
    #[error(transparent)]
    Player(#[from] PlayerSourceError),
    #[error("source instance not found: {source_instance_id}")]
    SourceInstanceNotFound { source_instance_id: String },
    #[error("source instance {source_instance_id} does not support {operation}")]
    UnsupportedSourceOperation {
        source_instance_id: String,
        operation: &'static str,
    },
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
    scheduler: RtCommandScheduler,
    rendered_frames: u64,
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
                scheduler: RtCommandScheduler::default(),
                rendered_frames: 0,
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
            .any(|channel| channel.legacy_source_id() == Some(source_id))
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

    pub fn add_file_source_instance(
        &mut self,
        request: FileSourceInstanceRequest,
    ) -> Result<(), SourceInstanceError> {
        request.source_instance_id.validate()?;
        let source_instance_id = request.source_instance_id.as_str();
        if source_instance_id.starts_with("legacy:") {
            return Err(SourceInstanceError::ReservedSourceInstanceId {
                source_instance_id: source_instance_id.to_string(),
            });
        }
        if self
            .channels
            .iter()
            .any(|channel| channel.source_instance_id() == &request.source_instance_id)
        {
            return Err(SourceInstanceError::DuplicateSourceInstance {
                source_instance_id: source_instance_id.to_string(),
            });
        }

        let mut source = PlayerSource::from_bytes(&request.bytes, self.sample_rate)?;
        let duration_frames = source.duration_frames() as u64;
        let duration_ms = frames_to_ms(duration_frames, self.sample_rate);
        let start_offset_frames = frames_for_duration_ms(request.start_offset_ms, self.sample_rate);
        if start_offset_frames >= duration_frames {
            return Err(SourceInstanceError::StartOffsetBeyondDuration {
                start_offset_ms: request.start_offset_ms,
                duration_ms,
            });
        }
        source.seek_frames(start_offset_frames);

        let timeline_start_ms = frames_to_ms(self.rendered_frames, self.sample_rate);
        let mut strip = ChannelStrip::new_timeline_source(
            request.source_instance_id,
            SourceKind::File,
            Some(SourceAssetRef::File {
                uri: request.uri,
                content_hash: None,
                duration_ms: Some(duration_ms),
            }),
            file_timeline(timeline_start_ms, request.start_offset_ms),
            Box::new(source),
            self.sample_rate,
        );
        strip.set_gain_db(request.gain_db, 0);
        strip.set_pan(request.pan, 0);
        strip.set_highpass_hz(request.highpass_hz);
        strip.set_lowpass_hz(request.lowpass_hz);
        self.channels.push(strip);
        Ok(())
    }

    pub fn stop_source_instance(
        &mut self,
        source_instance_id: &SourceInstanceId,
        fade_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.stop(fade_frames);
        Ok(())
    }

    pub fn set_source_instance_gain_db(
        &mut self,
        source_instance_id: &SourceInstanceId,
        db: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_gain_db(db, ramp_frames);
        Ok(())
    }

    pub fn set_source_instance_pan(
        &mut self,
        source_instance_id: &SourceInstanceId,
        pan: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_pan(pan, ramp_frames);
        Ok(())
    }

    pub fn set_source_instance_highpass_hz(
        &mut self,
        source_instance_id: &SourceInstanceId,
        hz: f32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_highpass_hz(hz);
        Ok(())
    }

    pub fn set_source_instance_lowpass_hz(
        &mut self,
        source_instance_id: &SourceInstanceId,
        hz: f32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_lowpass_hz(hz);
        Ok(())
    }

    pub fn set_source_instance_eq_gains_db(
        &mut self,
        source_instance_id: &SourceInstanceId,
        low_db: f32,
        mid_db: f32,
        high_db: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_eq_gains_db(
            finite_clamped_param(ParamId::EqLowGainDb, low_db, 0.0),
            finite_clamped_param(ParamId::EqMidGainDb, mid_db, 0.0),
            finite_clamped_param(ParamId::EqHighGainDb, high_db, 0.0),
            ramp_frames,
        );
        Ok(())
    }

    pub fn set_source_instance_eq_low_gain_db(
        &mut self,
        source_instance_id: &SourceInstanceId,
        low_db: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_eq_low_gain_db(
            finite_clamped_param(ParamId::EqLowGainDb, low_db, 0.0),
            ramp_frames,
        );
        Ok(())
    }

    pub fn set_source_instance_eq_mid_gain_db(
        &mut self,
        source_instance_id: &SourceInstanceId,
        mid_db: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_eq_mid_gain_db(
            finite_clamped_param(ParamId::EqMidGainDb, mid_db, 0.0),
            ramp_frames,
        );
        Ok(())
    }

    pub fn set_source_instance_eq_high_gain_db(
        &mut self,
        source_instance_id: &SourceInstanceId,
        high_db: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_eq_high_gain_db(
            finite_clamped_param(ParamId::EqHighGainDb, high_db, 0.0),
            ramp_frames,
        );
        Ok(())
    }

    pub fn set_source_instance_reverb_send_db(
        &mut self,
        source_instance_id: &SourceInstanceId,
        send_db: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        channel.set_reverb_send_db(
            finite_clamped_param(ParamId::ReverbSendDb, send_db, -60.0),
            ramp_frames,
        );
        Ok(())
    }

    pub fn set_source_instance_playback_rate(
        &mut self,
        source_instance_id: &SourceInstanceId,
        rate: f32,
        ramp_frames: u32,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        if !channel.set_playback_rate(sanitize_playback_rate(rate), ramp_frames) {
            return Err(SourceInstanceError::UnsupportedSourceOperation {
                source_instance_id: source_instance_id.as_str().to_string(),
                operation: "playback_rate",
            });
        }
        Ok(())
    }

    pub fn set_source_instance_reverse(
        &mut self,
        source_instance_id: &SourceInstanceId,
        reverse: bool,
    ) -> Result<(), SourceInstanceError> {
        let channel = self
            .channel_by_instance_id_mut(source_instance_id)
            .ok_or_else(|| SourceInstanceError::SourceInstanceNotFound {
                source_instance_id: source_instance_id.as_str().to_string(),
            })?;
        if !channel.set_reverse(reverse) {
            return Err(SourceInstanceError::UnsupportedSourceOperation {
                source_instance_id: source_instance_id.as_str().to_string(),
                operation: "reverse",
            });
        }
        Ok(())
    }

    pub fn automate_source_instance(
        &mut self,
        source_instance_id: &SourceInstanceId,
        ramp: SourceAutomationRamp,
    ) -> Result<(), SourceInstanceError> {
        match ramp.target {
            SourceAutomationTarget::GainDb => self.set_source_instance_gain_db(
                source_instance_id,
                finite_clamped_param(ParamId::GainDb, ramp.end_value, 0.0),
                ramp.duration_frames,
            ),
            SourceAutomationTarget::Pan => self.set_source_instance_pan(
                source_instance_id,
                finite_clamped_param(ParamId::Pan, ramp.end_value, 0.0),
                ramp.duration_frames,
            ),
            SourceAutomationTarget::EqLowGainDb => self.set_source_instance_eq_low_gain_db(
                source_instance_id,
                ramp.end_value,
                ramp.duration_frames,
            ),
            SourceAutomationTarget::EqMidGainDb => self.set_source_instance_eq_mid_gain_db(
                source_instance_id,
                ramp.end_value,
                ramp.duration_frames,
            ),
            SourceAutomationTarget::EqHighGainDb => self.set_source_instance_eq_high_gain_db(
                source_instance_id,
                ramp.end_value,
                ramp.duration_frames,
            ),
            SourceAutomationTarget::ReverbSendDb => self.set_source_instance_reverb_send_db(
                source_instance_id,
                ramp.end_value,
                ramp.duration_frames,
            ),
            SourceAutomationTarget::PlaybackRate => self.set_source_instance_playback_rate(
                source_instance_id,
                ramp.end_value,
                ramp.duration_frames,
            ),
        }
    }

    pub fn render_block(&mut self, output: &mut [StereoFrame]) {
        let drained_commands = self.drain_commands(MAX_DRAIN_PER_BLOCK);
        self.drain_scheduled_commands(MAX_DRAIN_PER_BLOCK.saturating_sub(drained_commands));

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
        self.rendered_frames = self.rendered_frames.saturating_add(output.len() as u64);
    }

    pub fn meters(&self) -> &MeterSnapshot {
        &self.last_meter
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Schedules an RT command against the runtime's current engine-frame position.
    ///
    /// Planned LLM/Pi origins must satisfy the protocol lead-time guard. Immediate
    /// manual/test/emergency origins may target the current frame. This is a
    /// non-render/control-side API because scheduler mutation may allocate or shift
    /// control structures; do not call it from the audio callback.
    pub fn schedule_rt_command(
        &mut self,
        request: RtCommandScheduleRequest,
    ) -> Result<(), RtCommandSchedulerError> {
        self.scheduler
            .schedule(request, self.rendered_frames, self.sample_rate)
    }

    /// Cancels a scheduled command from the non-render/control side.
    pub fn cancel_scheduled_rt_command(
        &mut self,
        action_id: &omm_protocol::ScheduledActionId,
    ) -> Result<crate::ScheduledRtCommand, RtCommandSchedulerError> {
        self.scheduler.cancel(action_id)
    }

    /// Modifies a scheduled command trigger from the non-render/control side.
    ///
    /// Planned origins are revalidated against the runtime's current engine frame.
    pub fn modify_scheduled_rt_command_trigger(
        &mut self,
        action_id: &omm_protocol::ScheduledActionId,
        new_trigger_frame: u64,
    ) -> Result<(), RtCommandSchedulerError> {
        self.scheduler.modify_trigger_frame(
            action_id,
            new_trigger_frame,
            self.rendered_frames,
            self.sample_rate,
        )
    }

    pub fn scheduled_rt_command_count(&self) -> usize {
        self.scheduler.len()
    }

    /// Reclaims scheduler metadata for already-dispatched commands.
    ///
    /// This is a non-render/control-side maintenance API and must not run on the
    /// audio callback path.
    pub fn reclaim_dispatched_scheduled_rt_commands(&mut self) -> usize {
        self.scheduler.reclaim_dispatched_metadata()
    }

    pub fn source_timeline_snapshot(&self) -> SourceTimelineSnapshot {
        SourceTimelineSnapshot {
            engine_frame: self.rendered_frames,
            sample_rate: self.sample_rate,
            sources: self
                .channels
                .iter()
                .map(|channel| {
                    channel.timeline_source_snapshot(self.rendered_frames, self.sample_rate)
                })
                .collect(),
        }
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
            .find(|ch| ch.legacy_source_id() == Some(source_id))
        {
            channel.set_gain_db(db, ramp_frames);
        }
    }

    pub fn set_channel_pan(&mut self, source_id: SourceId, pan: f32, ramp_frames: u32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.legacy_source_id() == Some(source_id))
        {
            channel.set_pan(pan, ramp_frames);
        }
    }

    pub fn set_channel_highpass_hz(&mut self, source_id: SourceId, hz: f32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.legacy_source_id() == Some(source_id))
        {
            channel.set_highpass_hz(hz);
        }
    }

    pub fn set_channel_lowpass_hz(&mut self, source_id: SourceId, hz: f32) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.legacy_source_id() == Some(source_id))
        {
            channel.set_lowpass_hz(hz);
        }
    }

    pub fn set_channel_enabled(&mut self, source_id: SourceId, enabled: bool) {
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|ch| ch.legacy_source_id() == Some(source_id))
        {
            channel.set_enabled(enabled);
        }
    }

    fn channel_by_instance_id_mut(
        &mut self,
        source_instance_id: &SourceInstanceId,
    ) -> Option<&mut ChannelStrip> {
        self.channels
            .iter_mut()
            .find(|channel| channel.source_instance_id() == source_instance_id)
    }

    fn drain_commands(&mut self, max: usize) -> usize {
        let mut count = 0;

        while count < max {
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

    fn drain_scheduled_commands(&mut self, max: usize) -> usize {
        let mut count = 0;

        while count < max {
            let Some(cmd) = self.scheduler.pop_next_due(self.rendered_frames) else {
                break;
            };

            self.apply_command(cmd);
            count += 1;
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
            RtCommand::SetSourceInstanceGainDb {
                source_instance_id,
                db,
                ramp_frames,
            } => {
                let _ = self.set_source_instance_gain_db(&source_instance_id, db, ramp_frames);
            }
            RtCommand::SetSourceInstancePan {
                source_instance_id,
                pan,
                ramp_frames,
            } => {
                let _ = self.set_source_instance_pan(&source_instance_id, pan, ramp_frames);
            }
            RtCommand::SetSourceInstanceHighpassHz {
                source_instance_id,
                hz,
            } => {
                let _ = self.set_source_instance_highpass_hz(&source_instance_id, hz);
            }
            RtCommand::SetSourceInstanceLowpassHz {
                source_instance_id,
                hz,
            } => {
                let _ = self.set_source_instance_lowpass_hz(&source_instance_id, hz);
            }
            RtCommand::SetSourceInstanceEq {
                source_instance_id,
                low_db,
                mid_db,
                high_db,
                ramp_frames,
            } => {
                let _ = self.set_source_instance_eq_gains_db(
                    &source_instance_id,
                    low_db,
                    mid_db,
                    high_db,
                    ramp_frames,
                );
            }
            RtCommand::SetSourceInstanceReverbSendDb {
                source_instance_id,
                send_db,
                ramp_frames,
            } => {
                let _ = self.set_source_instance_reverb_send_db(
                    &source_instance_id,
                    send_db,
                    ramp_frames,
                );
            }
            RtCommand::SetSourceInstancePlaybackRate {
                source_instance_id,
                rate,
                ramp_frames,
            } => {
                let _ =
                    self.set_source_instance_playback_rate(&source_instance_id, rate, ramp_frames);
            }
            RtCommand::SetSourceInstanceReverse {
                source_instance_id,
                reverse,
            } => {
                let _ = self.set_source_instance_reverse(&source_instance_id, reverse);
            }
        }
    }
}

fn finite_clamped_param(param: ParamId, value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        omm_protocol::validation::clamp_param(param, value)
    } else {
        fallback
    }
}

fn sanitize_playback_rate(rate: f32) -> f32 {
    if rate.is_finite() {
        rate.clamp(0.25, 4.0)
    } else {
        1.0
    }
}

fn frames_to_ms(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    ((frames as u128 * 1_000) / sample_rate as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::ChannelFeatures;
    use crate::source::{GlicolSource, TestToneSource};
    use omm_protocol::{
        ActionOrigin, EngineTime, PlaybackState, ScheduledActionId, SourceAssetRef, SourceId,
        SourceKind,
    };

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

    fn file_request(
        id: &str,
        uri: &str,
        bytes: &[u8],
        start_offset_ms: u64,
    ) -> FileSourceInstanceRequest {
        FileSourceInstanceRequest {
            source_instance_id: SourceInstanceId::new(id),
            uri: uri.to_string(),
            bytes: bytes.to_vec(),
            start_offset_ms,
            gain_db: 0.0,
            pan: 0.0,
            highpass_hz: 20.0,
            lowpass_hz: 20_000.0,
        }
    }

    fn mono_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let bytes_per_sample = 2_u16;
        let data_len = (samples.len() * bytes_per_sample as usize) as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * bytes_per_sample as u32).to_le_bytes());
        bytes.extend_from_slice(&bytes_per_sample.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());

        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        bytes
    }

    fn stepped_wav() -> Vec<u8> {
        let mut samples = vec![0_i16; SAMPLE_RATE as usize / 2];
        for (index, sample) in samples.iter_mut().enumerate().skip(4_800) {
            let phase = std::f32::consts::TAU * 440.0 * index as f32 / SAMPLE_RATE as f32;
            *sample = (phase.sin() * i16::MAX as f32 * 0.5) as i16;
        }
        mono_wav(SAMPLE_RATE, &samples)
    }

    fn sine_wav(frames: usize, freq_hz: f32) -> Vec<u8> {
        let samples: Vec<i16> = (0..frames)
            .map(|index| {
                let phase = std::f32::consts::TAU * freq_hz * index as f32 / SAMPLE_RATE as f32;
                (phase.sin() * i16::MAX as f32 * 0.5) as i16
            })
            .collect();
        mono_wav(SAMPLE_RATE, &samples)
    }

    fn impulse_wav(frames: usize) -> Vec<u8> {
        let mut samples = vec![0_i16; frames];
        if let Some(first) = samples.first_mut() {
            *first = (i16::MAX as f32 * 0.5) as i16;
        }
        mono_wav(SAMPLE_RATE, &samples)
    }

    fn ramp_wav(frames: usize) -> Vec<u8> {
        let samples: Vec<i16> = (0..frames)
            .map(|index| {
                let value = (index as f32 / frames.max(1) as f32) * 2.0 - 1.0;
                (value * i16::MAX as f32 * 0.5) as i16
            })
            .collect();
        mono_wav(SAMPLE_RATE, &samples)
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
    fn multiple_file_instances_can_share_the_same_asset_with_offsets() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let bytes = stepped_wav();
        let first_id = SourceInstanceId::new("file:loop-a");
        let second_id = SourceInstanceId::new("file:loop-b");

        runtime
            .add_file_source_instance(file_request(first_id.as_str(), "mem://same.wav", &bytes, 0))
            .expect("first file instance accepted");
        runtime
            .add_file_source_instance(file_request(
                second_id.as_str(),
                "mem://same.wav",
                &bytes,
                100,
            ))
            .expect("second file instance accepted with same asset");

        let snapshot = runtime.source_timeline_snapshot();
        let file_sources: Vec<_> = snapshot
            .sources
            .iter()
            .filter(|source| source.source_kind == SourceKind::File)
            .collect();
        assert_eq!(file_sources.len(), 2);
        assert!(file_sources.iter().all(|source| matches!(
            &source.asset_ref,
            Some(SourceAssetRef::File { uri, .. }) if uri == "mem://same.wav"
        )));
        let second = file_sources
            .iter()
            .find(|source| source.source_instance_id == second_id)
            .expect("second instance appears in snapshot");
        assert_eq!(second.playback.source_position_ms, Some(100));
        assert_eq!(
            second.timeline.active_windows[0].source_start_offset_ms,
            100
        );

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);
        let peak_value = peak(&output);
        assert!(
            peak_value > 0.2,
            "offset instance should start in the loud section, got peak {peak_value}"
        );
    }

    #[test]
    fn duplicate_and_invalid_file_source_instances_are_rejected() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let bytes = stepped_wav();

        runtime
            .add_file_source_instance(file_request("file:dup", "mem://dup.wav", &bytes, 0))
            .expect("first source accepted");
        assert!(matches!(
            runtime.add_file_source_instance(file_request("file:dup", "mem://dup.wav", &bytes, 0)),
            Err(SourceInstanceError::DuplicateSourceInstance { .. })
        ));
        assert!(matches!(
            runtime.add_file_source_instance(file_request("bad id", "mem://bad.wav", &bytes, 0)),
            Err(SourceInstanceError::Validation(
                SourceTimelineValidationError::InvalidSourceInstanceId { .. }
            ))
        ));
        assert!(matches!(
            runtime.add_file_source_instance(file_request(
                "legacy:mic",
                "mem://reserved.wav",
                &bytes,
                0
            )),
            Err(SourceInstanceError::ReservedSourceInstanceId { .. })
        ));
    }

    #[test]
    fn file_instance_start_offset_must_be_inside_file_duration() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let bytes = stepped_wav();

        let result = runtime.add_file_source_instance(file_request(
            "file:past-end",
            "mem://past-end.wav",
            &bytes,
            1_000,
        ));

        assert!(matches!(
            result,
            Err(SourceInstanceError::StartOffsetBeyondDuration {
                start_offset_ms: 1_000,
                duration_ms: 500
            })
        ));
        assert!(runtime.source_timeline_snapshot().sources.is_empty());
    }

    #[test]
    fn file_instance_controls_update_effect_status_and_rendering() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:controlled");
        let bytes = sine_wav(2_000, 440.0);

        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://controlled.wav", &bytes, 0))
            .expect("file source accepted");
        runtime
            .set_source_instance_gain_db(&id, -6.0, 0)
            .expect("gain control accepted");
        runtime
            .set_source_instance_pan(&id, -1.0, 0)
            .expect("pan control accepted");
        runtime
            .set_source_instance_highpass_hz(&id, 80.0)
            .expect("highpass control accepted");
        runtime
            .set_source_instance_lowpass_hz(&id, 12_000.0)
            .expect("lowpass control accepted");

        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("controlled source appears in snapshot");
        assert_eq!(source.effects.gain_db, -6.0);
        assert_eq!(source.effects.pan, -1.0);
        assert_eq!(source.effects.highpass_hz, 80.0);
        assert_eq!(source.effects.lowpass_hz, 12_000.0);

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);
        let left_peak = output
            .iter()
            .fold(0.0_f32, |current, frame| current.max(frame.left.abs()));
        let right_peak = output
            .iter()
            .fold(0.0_f32, |current, frame| current.max(frame.right.abs()));
        assert!(
            left_peak > 0.15,
            "left should remain audible, got {left_peak}"
        );
        assert!(
            right_peak < 0.01,
            "hard-left pan should remove right channel"
        );
    }

    #[test]
    fn file_instance_stop_fades_then_reports_stopped() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:fade-stop");
        let bytes = sine_wav(2_000, 440.0);

        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://fade.wav", &bytes, 0))
            .expect("file source accepted");
        runtime
            .stop_source_instance(&id, 128)
            .expect("stop with fade accepted");

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);
        assert!(peak(&output) > 0.01, "fade block should still be audible");

        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("stopped source appears in snapshot");
        assert_eq!(source.playback.state, PlaybackState::Stopped);

        output.fill(StereoFrame::SILENCE);
        runtime.render_block(&mut output);
        let stopped_peak = peak(&output);
        assert!(
            stopped_peak < 0.005,
            "stopped source should be silent, got {stopped_peak}"
        );
    }

    #[test]
    fn file_instance_eq_reverb_and_automation_update_status_and_audio() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:effects");
        let bytes = impulse_wav(2_000);

        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://impulse.wav", &bytes, 0))
            .expect("file source accepted");
        runtime
            .set_source_instance_eq_gains_db(&id, 6.0, -3.0, 4.0, 0)
            .expect("eq control accepted");
        runtime
            .set_source_instance_reverb_send_db(&id, 0.0, 0)
            .expect("reverb control accepted");
        runtime
            .automate_source_instance(
                &id,
                SourceAutomationRamp {
                    target: SourceAutomationTarget::GainDb,
                    end_value: -12.0,
                    duration_frames: 128,
                },
            )
            .expect("gain automation accepted");

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);
        assert!(
            output.iter().skip(2).any(|frame| frame.left.abs() > 0.01),
            "reverb should create a delayed source-local tail"
        );

        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("effects source appears in snapshot");
        assert!((source.effects.gain_db - -12.0).abs() < 0.001);
        assert_eq!(source.effects.eq.low_gain_db, 6.0);
        assert_eq!(source.effects.eq.mid_gain_db, -3.0);
        assert_eq!(source.effects.eq.high_gain_db, 4.0);
        assert_eq!(source.effects.reverb_send_db, 0.0);
    }

    #[test]
    fn independent_eq_band_automation_preserves_other_band_targets() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:eq-independent");
        let bytes = sine_wav(2_000, 440.0);

        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://eq.wav", &bytes, 0))
            .expect("file source accepted");
        runtime
            .automate_source_instance(
                &id,
                SourceAutomationRamp {
                    target: SourceAutomationTarget::EqLowGainDb,
                    end_value: 6.0,
                    duration_frames: 128,
                },
            )
            .expect("low EQ automation accepted");
        runtime
            .automate_source_instance(
                &id,
                SourceAutomationRamp {
                    target: SourceAutomationTarget::EqMidGainDb,
                    end_value: -3.0,
                    duration_frames: 128,
                },
            )
            .expect("mid EQ automation accepted");

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);

        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("EQ source appears in snapshot");
        assert!((source.effects.eq.low_gain_db - 6.0).abs() < 0.001);
        assert!((source.effects.eq.mid_gain_db - -3.0).abs() < 0.001);
        assert!((source.effects.eq.high_gain_db - 0.0).abs() < 0.001);
    }

    #[test]
    fn non_finite_playback_rate_falls_back_to_normal_speed() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:nan-rate");
        let bytes = sine_wav(2_000, 440.0);

        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://nan-rate.wav", &bytes, 0))
            .expect("file source accepted");
        runtime
            .set_source_instance_playback_rate(&id, f32::NAN, 0)
            .expect("NaN rate is sanitized, not fatal");

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);
        assert!(
            peak(&output) > 0.01,
            "sanitized rate should still render audio"
        );

        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("rate source appears in snapshot");
        assert_eq!(source.effects.playback_rate, 1.0);
    }

    #[test]
    fn file_instance_playback_rate_and_reverse_control_transport() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:transport-effects");
        let bytes = ramp_wav(12_000);

        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://ramp.wav", &bytes, 100))
            .expect("file source accepted");
        runtime
            .set_source_instance_playback_rate(&id, 2.0, 0)
            .expect("rate control accepted");
        let mut output = vec![StereoFrame::SILENCE; 512];
        runtime.render_block(&mut output);
        let fast_position = runtime
            .source_timeline_snapshot()
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .and_then(|source| source.playback.source_position_ms)
            .expect("source position reported after fast playback");
        assert!(
            fast_position >= 120,
            "2x playback should advance beyond 120ms, got {fast_position}"
        );

        runtime
            .set_source_instance_reverse(&id, true)
            .expect("reverse control accepted");
        runtime.render_block(&mut output);
        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("transport source appears in snapshot");
        let reversed_position = source
            .playback
            .source_position_ms
            .expect("source position reported after reverse playback");
        assert!(
            reversed_position < fast_position,
            "reverse playback should move backward from {fast_position}ms, got {reversed_position}"
        );
        assert_eq!(source.effects.playback_rate, 2.0);
        assert!(source.effects.reverse);
    }

    #[test]
    fn scheduled_source_instance_effect_command_applies_at_engine_time() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let id = SourceInstanceId::new("file:scheduled-effects");
        let bytes = sine_wav(2_000, 440.0);
        runtime
            .add_file_source_instance(file_request(id.as_str(), "mem://scheduled.wav", &bytes, 0))
            .expect("file source accepted");

        runtime
            .schedule_rt_command(RtCommandScheduleRequest {
                action_id: ScheduledActionId::new("schedule-source-eq"),
                origin: ActionOrigin::Test,
                trigger_frame: 0,
                command: RtCommand::SetSourceInstanceEq {
                    source_instance_id: id.clone(),
                    low_db: 4.0,
                    mid_db: -2.0,
                    high_db: 3.0,
                    ramp_frames: 0,
                },
            })
            .expect("source effect command scheduled");

        let mut output = vec![StereoFrame::SILENCE; 128];
        runtime.render_block(&mut output);

        let snapshot = runtime.source_timeline_snapshot();
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.source_instance_id == id)
            .expect("scheduled source appears in snapshot");
        assert_eq!(source.effects.eq.low_gain_db, 4.0);
        assert_eq!(source.effects.eq.mid_gain_db, -2.0);
        assert_eq!(source.effects.eq.high_gain_db, 3.0);
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
    fn immediate_and_scheduled_commands_share_one_render_budget() {
        let (mut runtime, mut queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Glicol,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        for _ in 0..MAX_DRAIN_PER_BLOCK {
            queue
                .enqueue(RtCommand::SetMasterGainDb {
                    db: -60.0,
                    ramp_frames: 0,
                })
                .expect("queue has capacity");
        }
        runtime
            .schedule_rt_command(RtCommandScheduleRequest {
                action_id: ScheduledActionId::new("restore-on-next-budget"),
                origin: ActionOrigin::Test,
                trigger_frame: 0,
                command: RtCommand::SetMasterGainDb {
                    db: 0.0,
                    ramp_frames: 0,
                },
            })
            .expect("due scheduled action accepted");

        let mut output = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut output);
        assert!(
            peak(&output) < 0.01,
            "scheduled action must wait when immediate queue consumes the block budget"
        );
        assert_eq!(runtime.scheduled_rt_command_count(), 1);

        runtime.render_block(&mut output);
        assert!(
            peak(&output) > 0.3,
            "scheduled action should drain on the next block once budget is available"
        );
        assert_eq!(runtime.scheduled_rt_command_count(), 0);
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
    fn runtime_applies_due_scheduled_commands_on_engine_frame() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Player,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );

        runtime
            .schedule_rt_command(RtCommandScheduleRequest {
                action_id: ScheduledActionId::new("mute-at-frame-256"),
                origin: ActionOrigin::Test,
                trigger_frame: 256,
                command: RtCommand::SetChannelEnabled {
                    source_id: SourceId::Player,
                    enabled: false,
                },
            })
            .expect("test action can be scheduled at an exact engine frame");

        let mut first = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut first);
        assert!(peak(&first) > 0.1, "source should play before trigger");
        assert_eq!(runtime.scheduled_rt_command_count(), 1);

        let mut second = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut second);
        let second_peak = peak(&second);
        assert!(
            second_peak < 0.05,
            "due command disables source at frame 256 after filter tail, got peak {second_peak}"
        );
        assert_eq!(runtime.scheduled_rt_command_count(), 0);
    }

    #[test]
    fn runtime_exposes_non_render_scheduler_reclaim() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        runtime
            .schedule_rt_command(RtCommandScheduleRequest {
                action_id: ScheduledActionId::new("reclaim-runtime"),
                origin: ActionOrigin::Test,
                trigger_frame: 0,
                command: RtCommand::SetMasterGainDb {
                    db: -6.0,
                    ramp_frames: 0,
                },
            })
            .expect("due scheduled action accepted");

        let mut output = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut output);
        assert_eq!(runtime.scheduled_rt_command_count(), 0);
        assert_eq!(runtime.reclaim_dispatched_scheduled_rt_commands(), 1);
        assert_eq!(runtime.reclaim_dispatched_scheduled_rt_commands(), 0);
    }

    #[test]
    fn runtime_rejects_planned_commands_inside_thirty_second_guard() {
        let (mut runtime, _queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        let minimum = EngineTime::new(0, SAMPLE_RATE).frame_after_ms(30_000);

        let early = runtime.schedule_rt_command(RtCommandScheduleRequest {
            action_id: ScheduledActionId::new("too-early"),
            origin: ActionOrigin::PlannedLlm,
            trigger_frame: minimum - 1,
            command: RtCommand::SetMasterGainDb {
                db: -6.0,
                ramp_frames: 0,
            },
        });
        assert!(early.is_err());
        assert_eq!(runtime.scheduled_rt_command_count(), 0);

        runtime
            .schedule_rt_command(RtCommandScheduleRequest {
                action_id: ScheduledActionId::new("planned-ok"),
                origin: ActionOrigin::PlannedLlm,
                trigger_frame: minimum,
                command: RtCommand::SetMasterGainDb {
                    db: -6.0,
                    ramp_frames: 0,
                },
            })
            .expect("planned LLM action at now+30s is accepted");
        assert_eq!(runtime.scheduled_rt_command_count(), 1);
    }

    #[test]
    fn runtime_reports_legacy_channels_as_timeline_source_instances() {
        let (mut runtime, mut queue, _handle) = AudioRuntime::new(AudioRuntimeConfig {
            sample_rate: SAMPLE_RATE,
        });
        add_test_channel(
            &mut runtime,
            SourceId::Player,
            Box::new(TestToneSource::new(440.0, SAMPLE_RATE)),
        );
        assert!(queue
            .enqueue(RtCommand::SetChannelGainDb {
                source_id: SourceId::Player,
                db: -9.0,
                ramp_frames: 0,
            })
            .is_ok());
        assert!(queue
            .enqueue(RtCommand::SetChannelPan {
                source_id: SourceId::Player,
                pan: 0.5,
                ramp_frames: 0,
            })
            .is_ok());

        let mut output = vec![StereoFrame::SILENCE; 256];
        runtime.render_block(&mut output);

        let snapshot = runtime.source_timeline_snapshot();
        assert_eq!(snapshot.engine_frame, 256);
        assert_eq!(snapshot.sample_rate, SAMPLE_RATE);
        assert_eq!(snapshot.sources.len(), 1);
        let source = &snapshot.sources[0];
        assert_eq!(source.source_instance_id.as_str(), "legacy:player");
        assert_eq!(source.source_kind, SourceKind::File);
        assert_eq!(source.playback.state, PlaybackState::Playing);
        assert_eq!(source.effects.gain_db, -9.0);
        assert_eq!(source.effects.pan, 0.5);
        assert_eq!(source.legacy_bridge.unwrap().source_id, SourceId::Player);
        assert!(source.timeline.is_active_at(0));
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
