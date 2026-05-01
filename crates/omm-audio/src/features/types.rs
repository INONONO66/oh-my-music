use omm_protocol::params::SourceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ChannelFeatures {
    pub source_id: SourceId,
    pub window_start_ms: u64,
    pub window_duration_ms: u32,
    // Dynamics
    pub peak_db: f32,
    pub rms_db: f32,
    pub crest_factor: f32,
    pub trend: Trend,
    // Spectral
    pub spectral_centroid_hz: f32,
    pub spectral_rolloff_hz: f32,
    pub spectral_flatness: f32,
    // Rhythm
    pub onset_rate_per_sec: f32,
    pub zero_crossing_rate: f32,
    // Labels
    pub energy_label: EnergyLabel,
    pub brightness_label: BrightnessLabel,
    pub texture_label: TextureLabel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Trend {
    Rising,
    Stable,
    Falling,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EnergyLabel {
    Silent,
    Quiet,
    Moderate,
    Loud,
    Peak,
}

impl EnergyLabel {
    pub fn from_rms_db(db: f32) -> Self {
        if db < -50.0 {
            EnergyLabel::Silent
        } else if db < -30.0 {
            EnergyLabel::Quiet
        } else if db < -12.0 {
            EnergyLabel::Moderate
        } else if db < -3.0 {
            EnergyLabel::Loud
        } else {
            EnergyLabel::Peak
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrightnessLabel {
    Dark,
    Warm,
    Neutral,
    Bright,
}

impl BrightnessLabel {
    pub fn from_spectral_centroid_hz(hz: f32) -> Self {
        if hz < 500.0 {
            BrightnessLabel::Dark
        } else if hz < 1500.0 {
            BrightnessLabel::Warm
        } else if hz < 4000.0 {
            BrightnessLabel::Neutral
        } else {
            BrightnessLabel::Bright
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextureLabel {
    Tonal,
    Mixed,
    Noisy,
}

impl TextureLabel {
    pub fn from_spectral_flatness(flatness: f32) -> Self {
        if flatness < 0.2 {
            TextureLabel::Tonal
        } else if flatness < 0.6 {
            TextureLabel::Mixed
        } else {
            TextureLabel::Noisy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_features_serializes_to_json() {
        let features = ChannelFeatures {
            source_id: SourceId::Glicol,
            window_start_ms: 0,
            window_duration_ms: 2000,
            peak_db: -6.0,
            rms_db: -20.0,
            crest_factor: 4.0,
            trend: Trend::Stable,
            spectral_centroid_hz: 2000.0,
            spectral_rolloff_hz: 8000.0,
            spectral_flatness: 0.3,
            onset_rate_per_sec: 2.5,
            zero_crossing_rate: 0.1,
            energy_label: EnergyLabel::Moderate,
            brightness_label: BrightnessLabel::Neutral,
            texture_label: TextureLabel::Mixed,
        };

        let json = serde_json::to_string(&features).expect("serialization failed");
        let deserialized: ChannelFeatures =
            serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(deserialized.source_id, SourceId::Glicol);
        assert_eq!(deserialized.window_start_ms, 0);
        assert_eq!(deserialized.window_duration_ms, 2000);
        assert_eq!(deserialized.peak_db, -6.0);
        assert_eq!(deserialized.rms_db, -20.0);
        assert_eq!(deserialized.crest_factor, 4.0);
        assert_eq!(deserialized.trend, Trend::Stable);
        assert_eq!(deserialized.spectral_centroid_hz, 2000.0);
        assert_eq!(deserialized.spectral_rolloff_hz, 8000.0);
        assert_eq!(deserialized.spectral_flatness, 0.3);
        assert_eq!(deserialized.onset_rate_per_sec, 2.5);
        assert_eq!(deserialized.zero_crossing_rate, 0.1);
        assert_eq!(deserialized.energy_label, EnergyLabel::Moderate);
        assert_eq!(deserialized.brightness_label, BrightnessLabel::Neutral);
        assert_eq!(deserialized.texture_label, TextureLabel::Mixed);
    }

    #[test]
    fn label_thresholds() {
        // Energy thresholds: Silent <-50, Quiet <-30, Moderate <-12, Loud <-3, Peak >=-3
        assert_eq!(EnergyLabel::from_rms_db(-60.0), EnergyLabel::Silent);
        assert_eq!(EnergyLabel::from_rms_db(-50.0), EnergyLabel::Quiet);
        assert_eq!(EnergyLabel::from_rms_db(-30.0), EnergyLabel::Moderate);
        assert_eq!(EnergyLabel::from_rms_db(-12.0), EnergyLabel::Loud);
        assert_eq!(EnergyLabel::from_rms_db(-3.0), EnergyLabel::Peak);
        assert_eq!(EnergyLabel::from_rms_db(0.0), EnergyLabel::Peak);

        // Brightness thresholds: Dark <500, Warm <1500, Neutral <4000, Bright >=4000
        assert_eq!(
            BrightnessLabel::from_spectral_centroid_hz(100.0),
            BrightnessLabel::Dark
        );
        assert_eq!(
            BrightnessLabel::from_spectral_centroid_hz(500.0),
            BrightnessLabel::Warm
        );
        assert_eq!(
            BrightnessLabel::from_spectral_centroid_hz(1500.0),
            BrightnessLabel::Neutral
        );
        assert_eq!(
            BrightnessLabel::from_spectral_centroid_hz(4000.0),
            BrightnessLabel::Bright
        );
        assert_eq!(
            BrightnessLabel::from_spectral_centroid_hz(8000.0),
            BrightnessLabel::Bright
        );

        // Texture thresholds: Tonal <0.2, Mixed <0.6, Noisy >=0.6
        assert_eq!(
            TextureLabel::from_spectral_flatness(0.1),
            TextureLabel::Tonal
        );
        assert_eq!(
            TextureLabel::from_spectral_flatness(0.2),
            TextureLabel::Mixed
        );
        assert_eq!(
            TextureLabel::from_spectral_flatness(0.6),
            TextureLabel::Noisy
        );
        assert_eq!(
            TextureLabel::from_spectral_flatness(0.9),
            TextureLabel::Noisy
        );
    }
}
