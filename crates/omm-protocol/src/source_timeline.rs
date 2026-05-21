use serde::{Deserialize, Serialize};

use crate::params::SourceId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct SourceInstanceId(String);

impl SourceInstanceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn validate(&self) -> Result<(), SourceTimelineValidationError> {
        validate_instance_id(self.as_str())
    }

    pub fn legacy(source_id: SourceId) -> Self {
        Self(format!("legacy:{}", legacy_source_slug(source_id)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SourceInstanceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for SourceInstanceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SourceKind {
    System,
    Mic,
    File,
    Generated,
}

impl SourceKind {
    pub fn from_legacy_source(source_id: SourceId) -> Self {
        match source_id {
            SourceId::System => Self::System,
            SourceId::Mic => Self::Mic,
            SourceId::Player => Self::File,
            SourceId::Glicol => Self::Generated,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceAssetRef {
    LiveInput {
        label: String,
    },
    File {
        uri: String,
        content_hash: Option<String>,
        duration_ms: Option<u64>,
    },
    Generated {
        engine: GeneratedEngine,
        code_ref: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GeneratedEngine {
    Glicol,
    Other { label: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimelineActiveWindow {
    pub timeline_start_ms: u64,
    pub timeline_end_ms: Option<u64>,
    pub source_start_offset_ms: u64,
}

impl TimelineActiveWindow {
    pub fn open_from_start() -> Self {
        Self {
            timeline_start_ms: 0,
            timeline_end_ms: None,
            source_start_offset_ms: 0,
        }
    }

    pub fn validate(&self) -> Result<(), SourceTimelineValidationError> {
        if let Some(end_ms) = self.timeline_end_ms {
            if end_ms <= self.timeline_start_ms {
                return Err(SourceTimelineValidationError::InvalidWindowRange {
                    timeline_start_ms: self.timeline_start_ms,
                    timeline_end_ms: end_ms,
                });
            }
        }
        Ok(())
    }

    pub fn is_active_at(&self, timeline_ms: u64) -> bool {
        timeline_ms >= self.timeline_start_ms
            && self
                .timeline_end_ms
                .map(|end_ms| timeline_ms < end_ms)
                .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTimelinePlacement {
    pub active_windows: Vec<TimelineActiveWindow>,
}

impl SourceTimelinePlacement {
    pub fn legacy_always_on() -> Self {
        Self {
            active_windows: vec![TimelineActiveWindow::open_from_start()],
        }
    }

    pub fn validate(&self) -> Result<(), SourceTimelineValidationError> {
        if self.active_windows.is_empty() {
            return Err(SourceTimelineValidationError::EmptyActiveWindows);
        }

        let mut previous_start_ms = None;
        let mut previous_end_ms = None;
        for (index, window) in self.active_windows.iter().enumerate() {
            window.validate()?;
            if let Some(start_ms) = previous_start_ms {
                if window.timeline_start_ms < start_ms {
                    return Err(SourceTimelineValidationError::UnsortedActiveWindows);
                }
            }
            if let Some(end_ms) = previous_end_ms {
                if window.timeline_start_ms < end_ms {
                    return Err(SourceTimelineValidationError::OverlappingActiveWindows);
                }
            }
            if window.timeline_end_ms.is_none() && index + 1 < self.active_windows.len() {
                return Err(SourceTimelineValidationError::OpenEndedWindowMustBeLast);
            }
            previous_start_ms = Some(window.timeline_start_ms);
            previous_end_ms = window.timeline_end_ms;
        }
        Ok(())
    }

    pub fn is_active_at(&self, timeline_ms: u64) -> bool {
        self.active_windows
            .iter()
            .any(|window| window.is_active_at(timeline_ms))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlaybackState {
    Pending,
    Queued,
    Playing,
    Paused,
    Stopped,
    Ended,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlaybackStatusAuthority {
    TimelineTransport,
    LegacyChannelEnabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourcePlaybackStatus {
    pub state: PlaybackState,
    pub authority: PlaybackStatusAuthority,
    pub timeline_position_ms: Option<u64>,
    pub source_position_ms: Option<u64>,
    pub loop_enabled: bool,
}

impl SourcePlaybackStatus {
    pub fn legacy_enabled(enabled: bool) -> Self {
        Self {
            state: if enabled {
                PlaybackState::Playing
            } else {
                PlaybackState::Stopped
            },
            authority: PlaybackStatusAuthority::LegacyChannelEnabled,
            timeline_position_ms: None,
            source_position_ms: None,
            loop_enabled: false,
        }
    }

    pub fn playing() -> Self {
        Self {
            state: PlaybackState::Playing,
            authority: PlaybackStatusAuthority::TimelineTransport,
            timeline_position_ms: None,
            source_position_ms: None,
            loop_enabled: false,
        }
    }

    pub fn stopped() -> Self {
        Self {
            state: PlaybackState::Stopped,
            authority: PlaybackStatusAuthority::TimelineTransport,
            timeline_position_ms: None,
            source_position_ms: None,
            loop_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceEffectStatus {
    pub gain_db: f32,
    pub pan: f32,
    pub highpass_hz: f32,
    pub lowpass_hz: f32,
    pub eq: SourceEqStatus,
    pub reverb_send_db: f32,
    pub playback_rate: f32,
    pub reverse: bool,
}

impl Default for SourceEffectStatus {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            highpass_hz: 20.0,
            lowpass_hz: 20_000.0,
            eq: SourceEqStatus::default(),
            reverb_send_db: -60.0,
            playback_rate: 1.0,
            reverse: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SourceEqStatus {
    pub low_gain_db: f32,
    pub mid_gain_db: f32,
    pub high_gain_db: f32,
}

impl Default for SourceEqStatus {
    fn default() -> Self {
        Self {
            low_gain_db: 0.0,
            mid_gain_db: 0.0,
            high_gain_db: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacySourceBridge {
    pub source_id: SourceId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineSourceInstance {
    pub source_instance_id: SourceInstanceId,
    pub source_kind: SourceKind,
    pub asset_ref: Option<SourceAssetRef>,
    pub timeline: SourceTimelinePlacement,
    pub playback: SourcePlaybackStatus,
    pub effects: SourceEffectStatus,
    pub legacy_bridge: Option<LegacySourceBridge>,
}

impl TimelineSourceInstance {
    pub fn validate(&self) -> Result<(), SourceTimelineValidationError> {
        self.source_instance_id.validate()?;
        self.timeline.validate()
    }

    pub fn legacy_channel(
        source_id: SourceId,
        playback: SourcePlaybackStatus,
        effects: SourceEffectStatus,
    ) -> Self {
        Self {
            source_instance_id: SourceInstanceId::legacy(source_id),
            source_kind: SourceKind::from_legacy_source(source_id),
            asset_ref: legacy_asset_ref(source_id),
            timeline: SourceTimelinePlacement::legacy_always_on(),
            playback,
            effects,
            legacy_bridge: Some(LegacySourceBridge { source_id }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceTimelineSnapshot {
    pub engine_frame: u64,
    pub sample_rate: u32,
    pub sources: Vec<TimelineSourceInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceTimelineValidationError {
    #[error("source_instance_id must not be empty")]
    EmptySourceInstanceId,
    #[error("source_instance_id contains unsupported character: {value}")]
    InvalidSourceInstanceId { value: String },
    #[error("timeline source requires at least one active window")]
    EmptyActiveWindows,
    #[error("timeline window end {timeline_end_ms} ms must be after start {timeline_start_ms} ms")]
    InvalidWindowRange {
        timeline_start_ms: u64,
        timeline_end_ms: u64,
    },
    #[error("active windows must be sorted by timeline_start_ms")]
    UnsortedActiveWindows,
    #[error("an open-ended active window must be the last window")]
    OpenEndedWindowMustBeLast,
    #[error("active windows must not overlap")]
    OverlappingActiveWindows,
}

fn validate_instance_id(value: &str) -> Result<(), SourceTimelineValidationError> {
    if value.is_empty() {
        return Err(SourceTimelineValidationError::EmptySourceInstanceId);
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '-' | '_' | '.'))
    {
        Ok(())
    } else {
        Err(SourceTimelineValidationError::InvalidSourceInstanceId {
            value: value.to_string(),
        })
    }
}

fn legacy_asset_ref(source_id: SourceId) -> Option<SourceAssetRef> {
    match source_id {
        SourceId::System => Some(SourceAssetRef::LiveInput {
            label: "system".to_string(),
        }),
        SourceId::Mic => Some(SourceAssetRef::LiveInput {
            label: "mic".to_string(),
        }),
        SourceId::Player => None,
        SourceId::Glicol => Some(SourceAssetRef::Generated {
            engine: GeneratedEngine::Glicol,
            code_ref: None,
        }),
    }
}

fn legacy_source_slug(source_id: SourceId) -> &'static str {
    match source_id {
        SourceId::System => "system",
        SourceId::Mic => "mic",
        SourceId::Player => "player",
        SourceId::Glicol => "glicol",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_source_instance_ids_are_stable() {
        assert_eq!(
            SourceInstanceId::legacy(SourceId::Mic).as_str(),
            "legacy:mic"
        );
        assert_eq!(
            SourceInstanceId::legacy(SourceId::Player).as_str(),
            "legacy:player"
        );
    }

    #[test]
    fn legacy_bridge_maps_fixed_sources_into_dynamic_kinds() {
        let player = TimelineSourceInstance::legacy_channel(
            SourceId::Player,
            SourcePlaybackStatus::playing(),
            SourceEffectStatus::default(),
        );
        let generated = TimelineSourceInstance::legacy_channel(
            SourceId::Glicol,
            SourcePlaybackStatus::stopped(),
            SourceEffectStatus::default(),
        );

        assert_eq!(player.source_kind, SourceKind::File);
        assert_eq!(player.source_instance_id.as_str(), "legacy:player");
        assert_eq!(player.legacy_bridge.unwrap().source_id, SourceId::Player);
        assert_eq!(generated.source_kind, SourceKind::Generated);
        assert!(matches!(
            generated.asset_ref,
            Some(SourceAssetRef::Generated {
                engine: GeneratedEngine::Glicol,
                ..
            })
        ));
    }

    #[test]
    fn active_windows_cover_timeline_ranges() {
        let placement = SourceTimelinePlacement {
            active_windows: vec![TimelineActiveWindow {
                timeline_start_ms: 30_000,
                timeline_end_ms: Some(45_000),
                source_start_offset_ms: 8_000,
            }],
        };

        assert!(!placement.is_active_at(29_999));
        assert!(placement.is_active_at(30_000));
        assert!(placement.is_active_at(44_999));
        assert!(!placement.is_active_at(45_000));
    }

    #[test]
    fn validation_rejects_invalid_instance_ids_and_windows() {
        assert_eq!(
            SourceInstanceId::new("").validate(),
            Err(SourceTimelineValidationError::EmptySourceInstanceId)
        );
        assert!(matches!(
            SourceInstanceId::new("bad id").validate(),
            Err(SourceTimelineValidationError::InvalidSourceInstanceId { .. })
        ));

        let empty = SourceTimelinePlacement {
            active_windows: Vec::new(),
        };
        assert_eq!(
            empty.validate(),
            Err(SourceTimelineValidationError::EmptyActiveWindows)
        );

        let reversed = TimelineActiveWindow {
            timeline_start_ms: 10_000,
            timeline_end_ms: Some(10_000),
            source_start_offset_ms: 0,
        };
        assert!(matches!(
            reversed.validate(),
            Err(SourceTimelineValidationError::InvalidWindowRange { .. })
        ));

        let open_then_later = SourceTimelinePlacement {
            active_windows: vec![
                TimelineActiveWindow {
                    timeline_start_ms: 10_000,
                    timeline_end_ms: None,
                    source_start_offset_ms: 0,
                },
                TimelineActiveWindow {
                    timeline_start_ms: 30_000,
                    timeline_end_ms: Some(40_000),
                    source_start_offset_ms: 0,
                },
            ],
        };
        assert_eq!(
            open_then_later.validate(),
            Err(SourceTimelineValidationError::OpenEndedWindowMustBeLast)
        );

        let overlapping = SourceTimelinePlacement {
            active_windows: vec![
                TimelineActiveWindow {
                    timeline_start_ms: 10_000,
                    timeline_end_ms: Some(20_000),
                    source_start_offset_ms: 0,
                },
                TimelineActiveWindow {
                    timeline_start_ms: 19_999,
                    timeline_end_ms: Some(30_000),
                    source_start_offset_ms: 0,
                },
            ],
        };
        assert_eq!(
            overlapping.validate(),
            Err(SourceTimelineValidationError::OverlappingActiveWindows)
        );
    }

    #[test]
    fn timeline_snapshot_round_trips_as_protocol_json() {
        let snapshot = SourceTimelineSnapshot {
            engine_frame: 960_000,
            sample_rate: 48_000,
            sources: vec![TimelineSourceInstance::legacy_channel(
                SourceId::Mic,
                SourcePlaybackStatus::playing(),
                SourceEffectStatus {
                    gain_db: -6.0,
                    pan: -0.25,
                    ..SourceEffectStatus::default()
                },
            )],
        };

        let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        assert!(json.contains("legacy:mic"));
        assert!(json.contains("Mic"));
        assert!(json.contains("gain_db"));
        let decoded: SourceTimelineSnapshot =
            serde_json::from_str(&json).expect("snapshot deserializes");

        assert_eq!(decoded, snapshot);
    }
}
