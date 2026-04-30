use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageSource {
    Engine,
    Agent,
    Tui,
    Discord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessagePriority {
    Realtime,
    Normal,
    Telemetry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope<T> {
    pub v: u16,
    pub msg_id: u64,
    pub corr_id: Option<u64>,
    pub session_id: String,
    pub source: MessageSource,
    pub kind: String,
    pub priority: MessagePriority,
    pub sent_at_ns: u64,
    pub body: T,
}
