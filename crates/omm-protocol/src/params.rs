use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SourceId {
    System,
    Mic,
    Player,
    Glicol,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum RtTarget {
    Master,
    Source(SourceId),
    Bus(BusId),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum BusId {
    System,
    Mic,
    Player,
    Glicol,
    Generated,
    FxReturn,
    Master,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ParamId {
    GainDb,
    Pan,
    Width,
    HighpassHz,
    LowpassHz,
    EqLowGainDb,
    EqMidGainDb,
    EqHighGainDb,
    ReverbSendDb,
    DelaySendDb,
    CompressorThresholdDb,
    CompressorRatio,
    DuckingAmountDb,
    MasterLimiterCeilingDb,
}
