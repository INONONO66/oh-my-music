use crate::params::ParamId;

pub fn clamp_param(param: ParamId, value: f32) -> f32 {
    match param {
        ParamId::GainDb => value.clamp(-60.0, 12.0),
        ParamId::Pan => value.clamp(-1.0, 1.0),
        ParamId::Width => value.clamp(0.0, 2.0),
        ParamId::HighpassHz => value.clamp(20.0, 1_000.0),
        ParamId::LowpassHz => value.clamp(1_000.0, 20_000.0),
        ParamId::EqLowGainDb | ParamId::EqMidGainDb | ParamId::EqHighGainDb => {
            value.clamp(-12.0, 12.0)
        }
        ParamId::ReverbSendDb | ParamId::DelaySendDb => value.clamp(-60.0, 0.0),
        ParamId::CompressorThresholdDb => value.clamp(-60.0, 0.0),
        ParamId::CompressorRatio => value.clamp(1.0, 20.0),
        ParamId::DuckingAmountDb => value.clamp(0.0, 24.0),
        ParamId::MasterLimiterCeilingDb => value.clamp(-6.0, -0.1),
    }
}
