use serde::{Deserialize, Serialize};

pub const PLANNED_ACTION_MIN_LEAD_MS: u64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct ScheduledActionId(String);

impl ScheduledActionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ScheduledActionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ScheduledActionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionOrigin {
    PlannedLlm,
    PlannedPi,
    Manual,
    Test,
    Emergency,
}

impl ActionOrigin {
    pub fn requires_planned_lead_time(self) -> bool {
        matches!(self, Self::PlannedLlm | Self::PlannedPi)
    }

    pub fn may_execute_immediately(self) -> bool {
        !self.requires_planned_lead_time()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineTime {
    pub frame: u64,
    pub sample_rate: u32,
}

impl EngineTime {
    pub fn new(frame: u64, sample_rate: u32) -> Self {
        Self {
            frame,
            sample_rate: sample_rate.max(1),
        }
    }

    pub fn frame_after_ms(&self, delta_ms: u64) -> u64 {
        self.frame
            .saturating_add(frames_for_duration_ms(delta_ms, self.sample_rate))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleRequestTiming {
    pub submitted_at_frame: u64,
    pub trigger_frame: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleValidation {
    pub action_id: ScheduledActionId,
    pub origin: ActionOrigin,
    pub timing: ScheduleRequestTiming,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleValidationError {
    #[error("scheduled action id must not be empty")]
    EmptyActionId,
    #[error(
        "planned action requires trigger_frame >= {minimum_trigger_frame}, got {trigger_frame}"
    )]
    PlannedActionTooSoon {
        trigger_frame: u64,
        minimum_trigger_frame: u64,
    },
}

pub fn validate_schedule_request(
    action_id: ScheduledActionId,
    origin: ActionOrigin,
    timing: ScheduleRequestTiming,
    sample_rate: u32,
) -> Result<ScheduleValidation, ScheduleValidationError> {
    if action_id.as_str().is_empty() {
        return Err(ScheduleValidationError::EmptyActionId);
    }

    if origin.requires_planned_lead_time() {
        let minimum_trigger_frame = EngineTime::new(timing.submitted_at_frame, sample_rate)
            .frame_after_ms(PLANNED_ACTION_MIN_LEAD_MS);
        if timing.trigger_frame < minimum_trigger_frame {
            return Err(ScheduleValidationError::PlannedActionTooSoon {
                trigger_frame: timing.trigger_frame,
                minimum_trigger_frame,
            });
        }
    }

    Ok(ScheduleValidation {
        action_id,
        origin,
        timing,
    })
}

pub fn frames_for_duration_ms(duration_ms: u64, sample_rate: u32) -> u64 {
    let numerator = duration_ms as u128 * sample_rate.max(1) as u128;
    numerator.div_ceil(1_000).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;

    fn timing(trigger_frame: u64) -> ScheduleRequestTiming {
        ScheduleRequestTiming {
            submitted_at_frame: 100,
            trigger_frame,
        }
    }

    #[test]
    fn planned_actions_require_thirty_second_lead_time() {
        let minimum = EngineTime::new(100, SAMPLE_RATE).frame_after_ms(30_000);

        let err = validate_schedule_request(
            ScheduledActionId::new("planned-early"),
            ActionOrigin::PlannedLlm,
            timing(minimum - 1),
            SAMPLE_RATE,
        )
        .expect_err("planned action before the minimum lead time must be rejected");

        assert_eq!(
            err,
            ScheduleValidationError::PlannedActionTooSoon {
                trigger_frame: minimum - 1,
                minimum_trigger_frame: minimum,
            }
        );

        assert!(validate_schedule_request(
            ScheduledActionId::new("planned-ok"),
            ActionOrigin::PlannedPi,
            timing(minimum),
            SAMPLE_RATE,
        )
        .is_ok());
    }

    #[test]
    fn immediate_origins_can_execute_at_or_before_now() {
        for origin in [
            ActionOrigin::Manual,
            ActionOrigin::Test,
            ActionOrigin::Emergency,
        ] {
            let accepted = validate_schedule_request(
                ScheduledActionId::new(format!("{origin:?}")),
                origin,
                ScheduleRequestTiming {
                    submitted_at_frame: 1_000,
                    trigger_frame: 0,
                },
                SAMPLE_RATE,
            )
            .expect("immediate origin accepted");

            assert_eq!(accepted.origin, origin);
        }
    }

    #[test]
    fn duration_to_frames_rounds_up() {
        assert_eq!(frames_for_duration_ms(30_000, SAMPLE_RATE), 1_440_000);
        assert_eq!(frames_for_duration_ms(1, 44_100), 45);
    }
}
