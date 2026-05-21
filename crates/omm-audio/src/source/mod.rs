pub mod glicol;
pub mod mic;
pub mod musical_test;
pub mod player;
pub mod test_tone;

pub use glicol::GlicolSource;
pub use mic::{MicSource, MicSourceError};
pub use musical_test::MusicalTestSource;
pub use player::{PlayerSource, PlayerSourceError};
pub use test_tone::TestToneSource;

use crate::frame::StereoFrame;

pub trait AudioSource: Send {
    fn render(&mut self, output: &mut [StereoFrame]);

    /// Disabled sources must write silence to `output`.
    fn set_enabled(&mut self, enabled: bool);

    /// `ramp_frames == 0` applies the new gain immediately.
    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32);

    fn position_frames(&self) -> Option<u64> {
        None
    }

    fn duration_frames(&self) -> Option<u64> {
        None
    }

    fn seek_frames(&mut self, _frame: u64) -> bool {
        false
    }

    fn is_finished(&self) -> bool {
        false
    }
}
