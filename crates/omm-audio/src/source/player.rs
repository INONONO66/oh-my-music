use std::io::Cursor;
use std::path::Path;

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{
    CodecParameters, CodecType, DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_PCM_F32LE,
    CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_S24LE, CODEC_TYPE_PCM_S32LE, CODEC_TYPE_PCM_U8,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

use crate::dsp::SmoothedParam;
use crate::frame::{db_to_gain, StereoFrame};
use crate::source::AudioSource;

const INTERNAL_CHANNELS: usize = 2;
const MAX_RESAMPLE_RATIO_RELATIVE: f64 = 1.0;

#[derive(Debug, thiserror::Error)]
pub enum PlayerSourceError {
    #[error("player target rate must be non-zero")]
    InvalidTargetRate,
    #[error("failed to read player source: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to decode player source: {0}")]
    Decode(String),
    #[error("decoded player source did not contain audio frames")]
    NoAudioFrames,
    #[error("decoded player source had no sample rate")]
    MissingSampleRate,
    #[error("failed to resample player source: {0}")]
    Resampler(String),
}

pub struct PlayerSource {
    frames: Vec<StereoFrame>,
    position: f64,
    enabled: bool,
    gain_db: SmoothedParam,
    playback_rate: SmoothedParam,
    reverse: bool,
}

impl PlayerSource {
    pub fn from_path(path: impl AsRef<Path>, target_rate: u32) -> Result<Self, PlayerSourceError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes, target_rate)
    }

    pub fn from_bytes(bytes: &[u8], target_rate: u32) -> Result<Self, PlayerSourceError> {
        if target_rate == 0 {
            return Err(PlayerSourceError::InvalidTargetRate);
        }

        let decoded = decode_frames(bytes)?;
        let frames = resample_frames(decoded.frames, decoded.sample_rate, target_rate)?;

        Ok(Self {
            frames,
            position: 0.0,
            enabled: true,
            gain_db: SmoothedParam::new(0.0),
            playback_rate: SmoothedParam::new(1.0),
            reverse: false,
        })
    }

    pub fn duration_frames(&self) -> usize {
        self.frames.len()
    }

    pub fn position_frames(&self) -> usize {
        self.clamped_position_frames()
    }

    pub fn is_finished(&self) -> bool {
        if self.reverse {
            self.position < 0.0
        } else {
            self.position >= self.frames.len() as f64
        }
    }

    pub fn restart(&mut self) {
        self.position = 0.0;
    }

    pub fn seek_frames(&mut self, frame: u64) {
        self.position = (frame as usize).min(self.frames.len()) as f64;
    }

    pub fn set_playback_rate(&mut self, rate: f32, ramp_frames: u32) {
        let rate = if rate.is_finite() {
            rate.clamp(0.25, 4.0)
        } else {
            1.0
        };
        self.playback_rate.set_target(rate, ramp_frames);
    }

    pub fn playback_rate(&self) -> f32 {
        self.playback_rate.current()
    }

    pub fn set_reverse(&mut self, reverse: bool) {
        if self.reverse != reverse {
            if reverse && (self.position <= 0.0 || self.position >= self.frames.len() as f64) {
                self.position = self.frames.len().saturating_sub(1) as f64;
            } else if !reverse && self.position < 0.0 {
                self.position = 0.0;
            }
        }
        self.reverse = reverse;
    }

    pub fn reverse(&self) -> bool {
        self.reverse
    }

    fn apply_gain(&mut self, frame: StereoFrame) -> StereoFrame {
        let gain = db_to_gain(self.gain_db.next_value());
        StereoFrame::new(frame.left * gain, frame.right * gain)
    }

    fn clamped_position_frames(&self) -> usize {
        if self.position <= 0.0 {
            0
        } else {
            (self.position.floor() as usize).min(self.frames.len())
        }
    }

    fn frame_at_position(&self, position: f64) -> Option<StereoFrame> {
        if !(0.0..self.frames.len() as f64).contains(&position) {
            return None;
        }

        let index = position.floor() as usize;
        let next_index = (index + 1).min(self.frames.len() - 1);
        let frac = (position - index as f64) as f32;
        let current = self.frames[index];
        let next = self.frames[next_index];
        Some(StereoFrame::new(
            current.left + (next.left - current.left) * frac,
            current.right + (next.right - current.right) * frac,
        ))
    }
}

impl AudioSource for PlayerSource {
    fn render(&mut self, output: &mut [StereoFrame]) {
        if output.is_empty() {
            return;
        }

        if !self.enabled {
            output.fill(StereoFrame::SILENCE);
            return;
        }

        let mut rendered = 0;
        for output_frame in output.iter_mut() {
            let Some(frame) = self.frame_at_position(self.position) else {
                break;
            };
            *output_frame = self.apply_gain(frame);
            let rate = {
                let next = self.playback_rate.next_value();
                if next.is_finite() {
                    next.clamp(0.25, 4.0)
                } else {
                    1.0
                }
            } as f64;
            if self.reverse {
                self.position -= rate;
            } else {
                self.position += rate;
            }
            rendered += 1;
        }

        if rendered < output.len() {
            output[rendered..].fill(StereoFrame::SILENCE);
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn set_gain_db(&mut self, gain_db: f32, ramp_frames: u32) {
        self.gain_db.set_target(gain_db, ramp_frames);
    }

    fn position_frames(&self) -> Option<u64> {
        Some(self.clamped_position_frames() as u64)
    }

    fn duration_frames(&self) -> Option<u64> {
        Some(self.frames.len() as u64)
    }

    fn seek_frames(&mut self, frame: u64) -> bool {
        PlayerSource::seek_frames(self, frame);
        true
    }

    fn is_finished(&self) -> bool {
        PlayerSource::is_finished(self)
    }

    fn set_playback_rate(&mut self, rate: f32, ramp_frames: u32) -> bool {
        PlayerSource::set_playback_rate(self, rate, ramp_frames);
        true
    }

    fn playback_rate(&self) -> Option<f32> {
        Some(PlayerSource::playback_rate(self))
    }

    fn set_reverse(&mut self, reverse: bool) -> bool {
        PlayerSource::set_reverse(self, reverse);
        true
    }

    fn reverse(&self) -> Option<bool> {
        Some(PlayerSource::reverse(self))
    }
}

struct DecodedFrames {
    frames: Vec<StereoFrame>,
    sample_rate: u32,
}

fn decode_frames(bytes: &[u8]) -> Result<DecodedFrames, PlayerSourceError> {
    let cursor = Cursor::new(bytes.to_vec());
    let media_source = MediaSourceStream::new(Box::new(cursor), Default::default());
    let probed = get_probe()
        .format(
            &Hint::new(),
            media_source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| PlayerSourceError::Decode(error.to_string()))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| PlayerSourceError::Decode("no supported audio track".to_string()))?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    if is_supported_pcm(codec_params.codec) {
        return decode_pcm_frames(format, track_id, &codec_params);
    }

    let mut decoder = get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|error| PlayerSourceError::Decode(error.to_string()))?;

    let mut frames = Vec::new();
    let mut sample_rate = codec_params.sample_rate;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(error) if is_end_of_stream(&error) => break,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(PlayerSourceError::Decode(error.to_string())),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                if spec.rate == 0 {
                    return Err(PlayerSourceError::MissingSampleRate);
                }
                sample_rate = Some(spec.rate);

                let mut sample_buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                sample_buffer.copy_interleaved_ref(decoded);
                append_stereo_frames(sample_buffer.samples(), spec.channels.count(), &mut frames);
            }
            Err(error) if is_end_of_stream(&error) => break,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(PlayerSourceError::Decode(error.to_string())),
        }
    }

    if frames.is_empty() {
        return Err(PlayerSourceError::NoAudioFrames);
    }

    let Some(sample_rate) = sample_rate else {
        return Err(PlayerSourceError::MissingSampleRate);
    };

    if sample_rate == 0 {
        return Err(PlayerSourceError::MissingSampleRate);
    }

    Ok(DecodedFrames {
        frames,
        sample_rate,
    })
}

fn decode_pcm_frames(
    mut format: Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    codec_params: &CodecParameters,
) -> Result<DecodedFrames, PlayerSourceError> {
    let Some(sample_rate) = codec_params.sample_rate else {
        return Err(PlayerSourceError::MissingSampleRate);
    };

    if sample_rate == 0 {
        return Err(PlayerSourceError::MissingSampleRate);
    }

    let channels = codec_params
        .channels
        .map(|channels| channels.count())
        .ok_or_else(|| PlayerSourceError::Decode("pcm track has no channel count".to_string()))?;
    let sample_bytes = pcm_sample_bytes(codec_params.codec)
        .ok_or_else(|| PlayerSourceError::Decode("unsupported pcm codec".to_string()))?;
    let frame_bytes = channels.saturating_mul(sample_bytes);
    if channels == 0 || frame_bytes == 0 {
        return Err(PlayerSourceError::Decode(
            "pcm track has invalid channel count".to_string(),
        ));
    }

    let mut frames = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(error) if is_end_of_stream(&error) => break,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(PlayerSourceError::Decode(error.to_string())),
        };

        if packet.track_id() != track_id {
            continue;
        }

        for source_frame in packet.data.chunks_exact(frame_bytes) {
            let left = read_pcm_sample(codec_params.codec, &source_frame[..sample_bytes])?;
            let right = if channels == 1 {
                left
            } else {
                let offset = sample_bytes;
                read_pcm_sample(
                    codec_params.codec,
                    &source_frame[offset..offset + sample_bytes],
                )?
            };
            frames.push(StereoFrame::new(left, right));
        }
    }

    if frames.is_empty() {
        return Err(PlayerSourceError::NoAudioFrames);
    }

    Ok(DecodedFrames {
        frames,
        sample_rate,
    })
}

fn is_supported_pcm(codec: CodecType) -> bool {
    matches!(
        codec,
        CODEC_TYPE_PCM_U8
            | CODEC_TYPE_PCM_S16LE
            | CODEC_TYPE_PCM_S24LE
            | CODEC_TYPE_PCM_S32LE
            | CODEC_TYPE_PCM_F32LE
    )
}

fn pcm_sample_bytes(codec: CodecType) -> Option<usize> {
    match codec {
        CODEC_TYPE_PCM_U8 => Some(1),
        CODEC_TYPE_PCM_S16LE => Some(2),
        CODEC_TYPE_PCM_S24LE => Some(3),
        CODEC_TYPE_PCM_S32LE | CODEC_TYPE_PCM_F32LE => Some(4),
        _ => None,
    }
}

fn read_pcm_sample(codec: CodecType, bytes: &[u8]) -> Result<f32, PlayerSourceError> {
    let sample = match codec {
        CODEC_TYPE_PCM_U8 => bytes[0] as f32 / 128.0 - 1.0,
        CODEC_TYPE_PCM_S16LE => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
        CODEC_TYPE_PCM_S24LE => {
            let raw = (bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16);
            ((raw << 8) >> 8) as f32 / 8_388_608.0
        }
        CODEC_TYPE_PCM_S32LE => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0
        }
        CODEC_TYPE_PCM_F32LE => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        _ => {
            return Err(PlayerSourceError::Decode(
                "unsupported pcm codec".to_string(),
            ));
        }
    };

    Ok(sanitize_sample(sample))
}

fn append_stereo_frames(samples: &[f32], channels: usize, frames: &mut Vec<StereoFrame>) {
    if channels == 0 {
        return;
    }

    for source_frame in samples.chunks_exact(channels) {
        let left = sanitize_sample(source_frame[0]);
        let right = if channels == 1 {
            left
        } else {
            sanitize_sample(source_frame[1])
        };
        frames.push(StereoFrame::new(left, right));
    }
}

fn resample_frames(
    frames: Vec<StereoFrame>,
    input_rate: u32,
    target_rate: u32,
) -> Result<Vec<StereoFrame>, PlayerSourceError> {
    if input_rate == target_rate {
        return Ok(frames);
    }

    if input_rate == 0 {
        return Err(PlayerSourceError::MissingSampleRate);
    }

    let input_len = frames.len();
    if input_len == 0 {
        return Ok(frames);
    }

    let ratio = target_rate as f64 / input_rate as f64;
    let params = SincInterpolationParameters {
        sinc_len: 64,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler = SincFixedIn::<f32>::new(
        ratio,
        MAX_RESAMPLE_RATIO_RELATIVE,
        params,
        input_len,
        INTERNAL_CHANNELS,
    )
    .map_err(|error| PlayerSourceError::Resampler(error.to_string()))?;

    let mut input = vec![Vec::with_capacity(input_len), Vec::with_capacity(input_len)];
    for frame in frames {
        input[0].push(frame.left);
        input[1].push(frame.right);
    }

    let output = resampler
        .process(&input, None)
        .map_err(|error| PlayerSourceError::Resampler(error.to_string()))?;
    let expected_len = resampled_len(input_len, input_rate, target_rate);

    let mut resampled = Vec::with_capacity(expected_len);
    let produced_len = output[0].len().min(output[1].len()).min(expected_len);
    for (left, right) in output[0].iter().zip(output[1].iter()).take(produced_len) {
        resampled.push(StereoFrame::new(
            sanitize_sample(*left),
            sanitize_sample(*right),
        ));
    }

    while resampled.len() < expected_len {
        resampled.push(StereoFrame::SILENCE);
    }

    Ok(resampled)
}

fn resampled_len(input_len: usize, input_rate: u32, target_rate: u32) -> usize {
    (input_len as u128 * target_rate as u128).div_ceil(input_rate as u128) as usize
}

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn is_end_of_stream(error: &SymphoniaError) -> bool {
    matches!(error, SymphoniaError::IoError(io_error) if io_error.kind() == std::io::ErrorKind::UnexpectedEof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const EPSILON: f32 = 0.0001;
    const MP3_HEX: &str = "fffbe240000007af903e85728000f73177b0ae3c005f9e4112197b0003d34821c32d600030a854c2a1530a854c220b4a23040a8c9c76350340df4f43a5ca4e22e935ca1ccae2b2605201186981c326271381be6c06c95019e2400c304f8531cf1cf20e4e225f27cbee821d32f97dd02e170ddd48208205c34374d3332f97cbe6eeb7a086b4d0a69a69ba904d3a08534d35a65f2fa69a7ad34d34e82081ba6f41065a65c416664d93e6ea40dd37ad32f97d36e9a69a75a69d0302e2140b85c3443a0665f37740c1064d0410e8350416665f27cddd04ea42b4d3d48170d1934d35a69d02e1a3265c65999bba90598170b868c9a6ea4d34d3d0d0302e1a32cccbe5f3465a69a6ea413a0820c9999ba14d3a0820ca41059997cbe6ee63b1b98bc526231398dca667f589b9e36747b29dced274b809c51a86c8651b29906c25a1a99306883d19a8b464a1b1888180f210831047c013809e31d203d03fdc938a082739a6e6c075a8e3302b2265fdf37befe294d7a4078f2997efe25dfabdfc7bff786c6af57abe3ee9b80f158f350dfbf794a5203c894f4a5358a5294feefd8d5efef8bfa3c7913de3eb3bc4078f299a5294a3c791298811299bdef7a7fffcdefaf4a51e5359a529fefd291efe94d7f8bdf78a53378f4cddfdfd203c7f1f7e9479129878ac562b191e53d2044a661c7dd1fb1bf7f1efbfffbddfbfbee97bef0f1e3c78f23ee8f1e5337dddfbf8f878f227b7c0000f488002200044600604a80720608104280600101780603a00b80600e80b806033007a06117020406410082c2e31e85ce396061728a1606210838201001c9a06eb13006ce4bc818c81e86e4f3136b8180a0620180080c0500a3c83ad3526907300c1000804c00858fdd04114d664ee1ef8ca1e2a93c852a08a6b49078cc0cc1a93e41cb0b4ecc9ba6eb3149062a150a66e9326ece82d37d23252e9d07333854272ee60826c9249a69eb4968ba689f652552e685c59f3740ea14199d265a9ed55968da925415456a42c7d37982a91bdd2520820c8ea5d3490a662c66821bf752cea2b65a2bb56956824a4d25a0ef5e8322e9d04745af532775dd47504d6060740b002895018010060302d818521f4060c122818792600b0620325416c0001881b2b40e06d94db818590f20280640c280380d56063c88e0284208d20c524c0d48152030ce2d4bc64e5248c40c0681a0b240b5d4992d3ac0280b80c00b1c83ca498dd92bae32075904cdcf51533217a478aa714554e9ba8fa174d15a064c824b496a75235a935aa89e3891928f294cc89b5ccac8549a0e924b3ca5a8c1f529682283ad0554e8b22e81f67774d13cea630baab32364eec62688aaa56834dd14d248fa29a492683ba2e9a0b5d140be9ba949b2d37345a9ab493524a4dd37f59ea0eb66412a4b7a2ad15a9041d2429be714f41d250000000000000000000000000000000000000fffbe260000007273847077b8000f38808c0eff4001e9a41020f78cdc3a6c82081ef19b830740c430d908e0806730140070400d982b812183c02998648b7194699e190f30c1af1eb21b84ebe9f2a40e98c31f81ac7ae21ab0a6d1889196990307e9aeeb47af8f1545a6443a99a07a61c06a9058c5d64c8304820380c2803010652b537804065ce6050318140c2100986836560204808b44d0d52cbad5ab96e96971a5997f61da072a2d331996d596e1976ad9c6b4a6cdc8d5aab4bccbf5955a5e56a6ed6a6b55696f56cbb9777cd65dee39771b2027e0d094240a808f02a56b3a2283425e1a110f0684a123da9e4961a23a8f02aa3a225bbf3cdd6e0e95782a59fd1fa9674b43a57d26001003a60090056600900306037816c607181d0950609483b46106851c1822d1891a62e1a69e5989abc03cd989261e41840606e1813a067180c608a989e62cf185460ef9dcfbc1e82f6186667995422989042992039989c158a03c61783c60300aac060482085689a92cab8bea60100e8648f602034c060157a31260ca54a4939930a1e8fac58843d00c0af0ae99c869fea3c6337b99d9ad6e334b84aa337a551acece78f3996f1c7f2a6bf2a977e34bcd65dfffc756b2dd2e5bab675dfcbfff5977f1c70ab8888f09759d0686074973a220a874157272cf054a9d3de25011d06bcaa8f1679df5151e77d2b7096ff9653fe596618414a60b603c6010002a98c1701fcc1d043cc6b0314c974f18d014d14d5f53d0e60f7a8f0dd2d8ce59544cb94a60d2419fcd4e891cc41c5f8c18c264c1a414cc28000cc24c258140ee50010f3a82beb0e33a5925dd4716f13a9a2a5ea82b6365b334d1f816d4cd5a6a7a5a94ddc79aa4c2d638d6e55adf9656b4d57d8bf13865231374a68274924a6b74f0bc2a892a5e386ffd9deb35e35b56f752c7ed420ce6e5e4f2725f29232a99197e9326d2cf5a6eeb19bfdcdede26e57cff4490c2aca9d7de6c6f518f2e76d9f9b9e52bca9606b956de26dd974936416d8fe72988c514856cf7f756adb7961d18e65dfb5aa57ce7531e6983981e2080c08c034c110060c2e40f0c210388c9fc570cde12ccc960e48c440e18d6db3dcee98c84d295280ce109f4d1d5758d16c7a0c2d0584c228250c138188c0c4264c3401ccaa0188254328d322a7725b92a652e7bd415e2654ba9e285c2245a9772556aace677b7677ac73b39636b3ffcf2b7fb57879b33bf35e62ad02d07766c2e103d4c795b113b5511ff68679c5533e54abdc6fdf520bee4e78ef1e6bedcc89dc9c6ef16bc6e6bfdbcecf50cdbada6e148157d2dac9576a664f6e73bdcab7a5fff6e6bf4fecf9cdbb678cbd6daf48e3edc6dbb5e3c9496d56cc15b9de6aab6b7615d3cb73ac00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000fffbe24000000734903c83af4b70e6b207f077466e1a9236ec0eb12dcba0479f01c4a5b938946a2621d7697ca3a5a859ab88c160d41c018f03844013a2a04e1a04789446439948851fefe2a1ce0a75455d21aad539d2ad771d0e8b26e02a93da7e9da3844a2b99dc60b6c15cb2610d3a5c9662bd48e9950447b0c4229b2d5cd865ccac2e5d0a642b11229f79339b4d7a45cb22662ac2d4451944cb1729cff4a5925c6c86488ba35e7140454ccd9490292d6cd1eb5519a6d0a492773d427904156a94afb3ec4ee308460b351ac7556ae8671452842bcd496cdf3661525bc2a916ab292cd4e48bbb7102028d1d4db77dd5e3f67e12db5668cad4d1975f71851f2a448a5b165a44452ca5b5534041608128880930141230441630540930a02231d5653cbafd349c5533a041330d373a114734d12f32a00e39a08d9192eab0b6e91dc7b410ec76670e43b85b8ceaaf72c2d674362d5aa58d72d5fb7dbf66e52d8fcb5bb9a9fe5cab4ef704944273127451131b8cd4b6d59e54029e7b35163856b9062ce2281b700b16924f1dacf2b520e9b3966691409617311648ce6a861a0c717d07375d45a21f4d149bce979917c09ecfe9c49bfb85e322458b73c8f9bb88332a5071ec412391042197193abbad543bd16c884a48be381b4e187e5bb9131243188d067baf36475062ec83fe90fe926f12c89c0224184c048082b040025a8120055f2173d0ecb2a686e84818096da0d0d551c0a8ed0d7915271e9a3cf995513274b5d3bc6ba52fbabce56662abcbcfc5118cc6d25c963c5f22d183913527c14a7734b229b0aad19cd496d154b4dc5cadfee9cd8c4da5a9db239db931e0f58b4db7094664be212bcc44d954d88b45923737912d3e6484ebb60aaaca19ac71d1eb79eba30562e55ea41b8aea636b205d1afb0575481b94d6a9c9c9cd49c233a84f549431ff2e928998eac44d22eb24f7a99396541b92537f42b34f821218b391bc85cfe6fb7596e1c9464b16569a4c99ac0e864cccb1013146010605139bdada6ce1d981414ace1c07d010292644cc939e5530f216104a0c11ad8620db39a8604257442c3f9cbcc67da30c9a5645a4b284b0111145522196a11230ee32d8a8951ca040aa2a68fc1d6b32c923642c3a316122ae588b5ca1627544b73259c2279256315cd409a0a8d22592a2e722a499d6565958396c8d0ab92a5b6e8a68b5f694558226da0f75420bb75e9ec8889d5744f65cdd4b9569a666caf54f8a6e2aefb6c59932b132eb528b5c48dde6a30da44230bc19b4a41640f617594928ab528cda590aa631b25489bb91c9a51382aa685800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000fffbe2400000075a9039838f4b70edd207806ce96e1abe2ce20d3d2dcba7481cc1a625b83ad978c603031187cc2c07048413a503de45904a04646952d29863073f45d0a9785d59c9420ccd3015afd94ff391447cb52c4eaf3fd814cbefd61b93ec1dba667715c2bed176e0e6f5b2ee50591c372e1c4c5cc3f3bccac8dd889835151b9b2a0fb53845eaa03f17f3cd2648d4d96ccc91b14f54c1951bc61a462c8987bd9489cca86a0690a03eab96364cb989368543442db38d2660997666cacdb969c5842a371ad9b2b28fbca9b294ea76a36aa5a9ea681a4e0936aaeea9da8d26c461e6c497856427949f84a13c5aee125e715bde2cbe47d5a12d3435149a429349aa73112b15b4aa8896ead2cab9144f9cd8c500dd98db7b00b9c90c84d7649068ce4b8783d1a1f30c864181c0141e0a00e0020061000e11447084603c90d8168120c894110bd0640146c2b158606a2c00a1d30f2c2a64170e242207c8251201cba41a4b5085702c1a34d150aea12d706c955431116b13288994c552154c69b81a4086415591a03ec06929b48586d5ae92d2a456341a9c64888a7979ad90b88604d9a43b9bb58891a0425a911730d39e449c50a09c1138861b1a262a66738a2fa54cf967eb11136a6cc91469b776652559f992d656457642850b13a43fc55a7e2b05ae2b4a9fd5650b3ea492185f9bb48528db31fa89a8c3c65159b8153977435f861b0c802a190168ca9ccd2908a489012e48ba50e09899a04cd41311a900e35f4e2e54cd4a98e9798e89cfcca4df2e953052d0d6db558dcbf055d8598ab1195f1571b5cbe61728cb2a0f9d4058b2c43cd44c2c4722f659c84d347a661246aa5243d9a6ed4a3b359b5988b76c54eafedb14bf5f52726fa629892f95b1a6e97b298775265233df1625718e3ed8a476534e4536a99ef96c6a598df6fa0ebf52371b97c978ec7698e8fa0b52339547edfb953adb8310526bd546e5f6e095dca9ac76b78a4d3b8cb630d4e09d546dac704276314e7de5005a1ad149985940b880b80570d61b8beb28702490c01c1408476d8fa36708836514364ce447858716935538844a4250c2165185d43c4232ce74f6258c554b8ba24d87a65482a42d418388b19f2a492492849ad8419728b6ab84a6525908a60891889a42652263a29812a01134ab2b133645242e59abb932ec95a18c9ea2d342cc9a2a29892a82a6c8594468e0aac9602a6c870526566934344cda29a16514cac658b3571d7a687b3489eaf44fa95abda7ab8859ad5d58c9f26934388a6ab2ab32266d0e113c89a3a856269a1810b289e550ac893693438d4d0b2b451349c566956538c9a9c6915800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000fffbe2400000078f8f37834c3372e072071069e96e00a806de0c8c000820815bc1a304013b4f8edb42a9f3504c946193085802061a9f882ad193a9bab4d7d8dc492008a420a852148a80d0a40c8f214978462e93508fb6d5c79974e95154f8e4f531f427b01f62efaf67cee4d6ac939182304964544966ece56cb69ba6a892c1540cb06582a8959184a923544960aa064004980500126013089848d44e582a001400498048008f0096125051a89c902ac150011e0130042c045848105091a2960c982c008f00941428284848a0a123452249646091e120412282848d061c0a341540c9918245a26a27240a8832c1560aa25646091691a89cb22a2564ac8c12b22c9354e55cc12624e8b24e8b4b6cfa793a4ed370ed140b20753598a38212e2c650920d0c4a0cbe89f2a62c290e2af2168a2621f46a29e20c127871ac5c912713e8d4ea48ff502393cb83dd031e45f4425a04444aa16dd6524b6c8cac2d8af68666e7ea454ee8de4826868d1e3ecd943ee262cc3665a59b604de0b9e26cd48d32cdc0d328967b04b2a7b89b754328bec09a32939146526994574d328b658895bf88ae52690ab526a2b6f8aab6cb115c3c514ea4d2a94fad696d4564f656b4e1e4ace1ab270deada4d55a53cbb59f9e49ba7ab4e1bd584daab4a71ba4df9b5376e2b384bdc26cc6707c6e136776a6cee26ed955e6c6304c4e975848cf078d62424c1d206b000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    fn mp3_bytes() -> Vec<u8> {
        MP3_HEX
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]))
            .collect()
    }

    fn hex_nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => panic!("invalid hex fixture byte"),
        }
    }

    fn render(source: &mut PlayerSource, frames: usize) -> Vec<StereoFrame> {
        let mut output = vec![StereoFrame::SILENCE; frames];
        source.render(&mut output);
        output
    }

    fn peak(frames: &[StereoFrame]) -> f32 {
        frames.iter().fold(0.0_f32, |current, frame| {
            current.max(frame.left.abs()).max(frame.right.abs())
        })
    }

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {actual} to be within {EPSILON} of {expected}"
        );
    }

    fn stereo_wav(sample_rate: u32, frames: usize, freq_hz: f32) -> Vec<u8> {
        let mut samples = Vec::with_capacity(frames * 2);
        for frame_index in 0..frames {
            let phase = TAU * freq_hz * frame_index as f32 / sample_rate as f32;
            let sample = (phase.sin() * i16::MAX as f32 * 0.5) as i16;
            samples.push(sample);
            samples.push(-sample);
        }

        wav_bytes(sample_rate, 2, &samples)
    }

    fn mono_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        wav_bytes(sample_rate, 1, samples)
    }

    fn wav_bytes(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let bytes_per_sample = 2_u16;
        let data_len = (samples.len() * bytes_per_sample as usize) as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(
            &(sample_rate * channels as u32 * bytes_per_sample as u32).to_le_bytes(),
        );
        bytes.extend_from_slice(&(channels * bytes_per_sample).to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());

        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        bytes
    }

    fn estimate_frequency_hz(frames: &[StereoFrame], sample_rate: u32) -> f32 {
        let crossings = frames
            .windows(2)
            .filter(|pair| pair[0].left <= 0.0 && pair[1].left > 0.0)
            .count();

        crossings as f32 * sample_rate as f32 / frames.len() as f32
    }

    #[test]
    fn wav_decode_44100_to_48000() -> Result<(), PlayerSourceError> {
        let bytes = stereo_wav(44_100, 44_100, 1_000.0);
        let mut source = PlayerSource::from_bytes(&bytes, 48_000)?;

        assert!(
            source.duration_frames().abs_diff(48_000) <= 192,
            "expected ≈48000 frames, got {}",
            source.duration_frames()
        );
        let duration = source.duration_frames();
        let output = render(&mut source, duration);
        let frequency_hz = estimate_frequency_hz(&output, 48_000);

        assert!(peak(&output) > 0.25);
        assert!(
            (frequency_hz - 1_000.0).abs() <= 25.0,
            "expected ≈1000Hz, got {frequency_hz}Hz"
        );
        assert!(source.is_finished());
        Ok(())
    }

    #[test]
    fn mp3_decode_basic() -> Result<(), PlayerSourceError> {
        let bytes = mp3_bytes();
        let mut source = PlayerSource::from_bytes(&bytes, 48_000)?;
        let duration = source.duration_frames();

        assert!(duration > 0);
        let output = render(&mut source, duration);

        assert!(peak(&output) > 0.0);
        assert!(source.is_finished());
        Ok(())
    }

    #[test]
    fn corrupted_bytes_returns_err() {
        assert!(PlayerSource::from_bytes(b"NOT_AUDIO", 48_000).is_err());
    }

    #[test]
    fn mono_upmixed_to_stereo() -> Result<(), PlayerSourceError> {
        let bytes = mono_wav(48_000, &[8_192, -8_192, 16_384]);
        let mut source = PlayerSource::from_bytes(&bytes, 48_000)?;

        let output = render(&mut source, 3);

        for frame in output {
            assert_near(frame.left, frame.right);
        }
        Ok(())
    }

    #[test]
    fn is_finished_after_full_playback() -> Result<(), PlayerSourceError> {
        let bytes = stereo_wav(48_000, 8, 440.0);
        let mut source = PlayerSource::from_bytes(&bytes, 48_000)?;

        let output = render(&mut source, 12);

        assert_eq!(source.position_frames(), source.duration_frames());
        assert!(source.is_finished());
        for frame in &output[8..] {
            assert_near(frame.left, 0.0);
            assert_near(frame.right, 0.0);
        }
        Ok(())
    }

    #[test]
    fn switching_reverse_off_after_reverse_finish_resumes_forward_from_start(
    ) -> Result<(), PlayerSourceError> {
        let bytes = stereo_wav(48_000, 16, 440.0);
        let mut source = PlayerSource::from_bytes(&bytes, 48_000)?;

        source.set_reverse(true);
        let reverse_output = render(&mut source, 20);
        assert!(source.is_finished());
        assert!(reverse_output
            .iter()
            .take(16)
            .any(|frame| frame.left.abs() > 0.0));

        source.set_reverse(false);
        assert_eq!(source.position_frames(), 0);
        assert!(!source.is_finished());
        let forward_output = render(&mut source, 4);

        assert!(
            forward_output.iter().any(|frame| frame.left.abs() > 0.0),
            "forward playback should resume after toggling reverse off"
        );
        Ok(())
    }

    #[test]
    fn restart_replays_from_beginning() -> Result<(), PlayerSourceError> {
        let bytes = stereo_wav(48_000, 16, 440.0);
        let mut source = PlayerSource::from_bytes(&bytes, 48_000)?;
        let first = render(&mut source, 4);

        source.restart();
        let replayed = render(&mut source, 4);

        for (left, right) in first.iter().zip(replayed.iter()) {
            assert_near(left.left, right.left);
            assert_near(left.right, right.right);
        }
        Ok(())
    }
}
