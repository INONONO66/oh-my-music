use serde::{Deserialize, Serialize};

use crate::params::{ParamId, RtTarget, SourceId};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionMode {
    Tui,
    Discord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutputRoute {
    LocalCpal { device_id: Option<String> },
    DiscordPcmIpc { client_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetParam {
    pub seq: u64,
    pub target: RtTarget,
    pub param: ParamId,
    pub value: f32,
    pub ramp_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EngineCommand {
    Hello { client_name: String, client_version: String },
    StartSession { mode: SessionMode, output_route: OutputRoute },
    StopSession { fade_out_ms: u32 },
    SetCapture { system_audio: bool, mic: bool, exclude_own_process: bool },
    SetParam(SetParam),
    SetParamBatch { seq: u64, commands: Vec<SetParam>, reason: String },
    SetSourceMute { source: SourceId, muted: bool, ramp_ms: u32 },
    GlicolLoadCode { code: String, transition_ms: u32 },
    EmergencyFade { fade_ms: u32, reason: String },
    RequestState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EngineEvent {
    HelloAck { engine_version: String, sample_rate: u32 },
    Ack { acked_msg_id: u64, applied_seq: Option<u64> },
    Nack { rejected_msg_id: u64, code: String, message: String },
    StateSnapshot { engine_frame: u64, sample_rate: u32 },
    MeterFrame { engine_frame: u64, master_peak_db: f32, master_rms_db: f32 },
    CaptureStatus { system_audio: String, mic: String },
}
