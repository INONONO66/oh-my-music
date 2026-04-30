pub mod glicol;
pub mod test_tone;

pub use glicol::GlicolSource;
pub use test_tone::TestToneSource;

use crate::frame::StereoFrame;

pub trait AudioSource: Send {
    fn render(&mut self, output: &mut [StereoFrame]);

    /// Disabled sources must write silence to `output`.
    fn set_enabled(&mut self, enabled: bool);

    /// `ramp_frames == 0` applies the new gain immediately.
    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32);
}
