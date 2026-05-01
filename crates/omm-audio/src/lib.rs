pub mod channel;
pub mod command;
pub mod constants;
pub mod dsp;
pub mod features;
pub mod frame;
pub mod meter;
pub mod mixer;
pub mod output;
pub mod runtime;
pub mod source;

pub use channel::ChannelStrip;
pub use command::{
    new_command_channel, CommandQueue, CommandReceiver, QueueFull, RtCommand, MAX_DRAIN_PER_BLOCK,
    RT_QUEUE_CAPACITY,
};
pub use constants::{ENGINE_CHANNELS, ENGINE_SAMPLE_RATE, MAX_BLOCK_FRAMES};
pub use features::{
    BrightnessLabel, ChannelFeatures, EnergyLabel, FeatureAnalyzerHandle, TextureLabel, Trend,
};
pub use frame::StereoFrame;
pub use meter::MeterSnapshot;
pub use runtime::{AudioRuntime, AudioRuntimeConfig};

#[cfg(test)]
mod compile_checks {
    fn _assert_send<T: Send>() {}

    #[test]
    fn _check_glicol_engine_is_send() {
        _assert_send::<glicol::Engine<128>>();
    }
}
