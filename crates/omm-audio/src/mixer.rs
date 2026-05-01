use crate::constants::MAX_BLOCK_FRAMES;
use crate::frame::StereoFrame;
use crate::ChannelStrip;

/// Sums rendered channel-strip outputs into a single stereo bus.
///
/// Master gain, pan, and post-processing are owned by `AudioRuntime`; the
/// mixer is intentionally a pure summing stage. `MAX_BLOCK_FRAMES` of scratch
/// capacity is pre-allocated so steady-state `render()` does not allocate.
pub struct Mixer {
    scratch: Vec<StereoFrame>,
}

impl Mixer {
    pub fn new() -> Self {
        Self {
            scratch: Vec::with_capacity(MAX_BLOCK_FRAMES),
        }
    }

    pub fn render(&mut self, channels: &mut [ChannelStrip], output: &mut [StereoFrame]) {
        if output.is_empty() {
            return;
        }

        let n = output.len();

        for frame in output.iter_mut() {
            *frame = StereoFrame::SILENCE;
        }

        self.scratch.resize(n, StereoFrame::SILENCE);

        for channel in channels.iter_mut() {
            channel.render(&mut self.scratch[..n]);
            for (out_frame, channel_frame) in output.iter_mut().zip(self.scratch[..n].iter()) {
                out_frame.left += channel_frame.left;
                out_frame.right += channel_frame.right;
            }
        }
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TestToneSource;
    use omm_protocol::SourceId;

    const SAMPLE_RATE: u32 = 48_000;
    const FRAME_COUNT: usize = 128;

    fn peak(buf: &[StereoFrame]) -> f32 {
        buf.iter()
            .fold(0.0_f32, |acc, f| acc.max(f.left.abs()).max(f.right.abs()))
    }

    fn test_channel(source_id: SourceId, freq_hz: f32) -> ChannelStrip {
        ChannelStrip::new(
            source_id,
            Box::new(TestToneSource::new(freq_hz, SAMPLE_RATE)),
            SAMPLE_RATE,
        )
    }

    #[test]
    fn test_render_zero_sources_produces_silence() {
        let mut mixer = Mixer::new();
        let mut output = vec![StereoFrame::new(0.9, -0.9); FRAME_COUNT];
        let mut channels = Vec::new();
        mixer.render(&mut channels, &mut output);
        for (i, f) in output.iter().enumerate() {
            assert_eq!(f.left, 0.0, "left non-zero at {}: {}", i, f.left);
            assert_eq!(f.right, 0.0, "right non-zero at {}: {}", i, f.right);
        }
    }

    #[test]
    fn test_render_single_source_produces_signal() {
        let mut mixer = Mixer::new();
        let mut channels = vec![test_channel(SourceId::Glicol, 440.0)];
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        mixer.render(&mut channels, &mut output);
        let p = peak(&output);
        assert!(p > 0.5, "expected peak > 0.5, got {}", p);
    }

    #[test]
    fn test_render_two_sources_sum() {
        let mut mixer = Mixer::new();
        let mut channels = vec![
            test_channel(SourceId::System, 440.0),
            test_channel(SourceId::Glicol, 880.0),
        ];
        let mut output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        mixer.render(&mut channels, &mut output);
        let p = peak(&output);
        assert!(p > 0.5, "expected summed peak > 0.5, got {}", p);
    }

    #[test]
    fn test_render_single_channel_matches_channel_strip_output() {
        let mut mixer = Mixer::new();
        let mut direct = test_channel(SourceId::Glicol, 440.0);
        let mut channels = vec![test_channel(SourceId::Glicol, 440.0)];
        let mut direct_output = vec![StereoFrame::SILENCE; FRAME_COUNT];
        let mut mixed_output = vec![StereoFrame::SILENCE; FRAME_COUNT];

        direct.render(&mut direct_output);
        mixer.render(&mut channels, &mut mixed_output);

        for (index, (direct_frame, mixed_frame)) in
            direct_output.iter().zip(mixed_output.iter()).enumerate()
        {
            assert!(
                (direct_frame.left - mixed_frame.left).abs() < 0.000001,
                "left differs at {index}: direct {}, mixed {}",
                direct_frame.left,
                mixed_frame.left
            );
            assert!(
                (direct_frame.right - mixed_frame.right).abs() < 0.000001,
                "right differs at {index}: direct {}, mixed {}",
                direct_frame.right,
                mixed_frame.right
            );
        }
    }

    #[test]
    fn test_render_empty_output_does_not_panic() {
        let mut mixer = Mixer::new();
        let mut channels = vec![test_channel(SourceId::Glicol, 440.0)];
        let mut output: Vec<StereoFrame> = Vec::new();
        mixer.render(&mut channels, &mut output);
        assert!(output.is_empty());
    }

    #[test]
    fn test_default_constructs_mixer() {
        let _mixer = Mixer::default();
    }
}
