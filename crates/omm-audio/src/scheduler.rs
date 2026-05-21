use std::collections::BTreeMap;

use omm_protocol::{
    validate_schedule_request, ActionOrigin, ScheduleRequestTiming, ScheduleValidationError,
    ScheduledActionId,
};

use crate::command::RtCommand;

#[derive(Debug, Clone, PartialEq)]
pub struct RtCommandScheduleRequest {
    pub action_id: ScheduledActionId,
    pub origin: ActionOrigin,
    pub trigger_frame: u64,
    pub command: RtCommand,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledRtCommand {
    pub action_id: ScheduledActionId,
    pub origin: ActionOrigin,
    pub trigger_frame: u64,
    pub command: RtCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduledRtCommandState {
    Pending,
    Dispatched,
}

#[derive(Debug, Clone, PartialEq)]
struct ScheduledRtCommandEntry {
    action: ScheduledRtCommand,
    state: ScheduledRtCommandState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScheduledDueKey {
    trigger_frame: u64,
    action_id: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RtCommandSchedulerError {
    #[error(transparent)]
    Validation(#[from] ScheduleValidationError),
    #[error("scheduled action already exists: {action_id}")]
    DuplicateAction { action_id: String },
    #[error("scheduled action not found: {action_id}")]
    ActionNotFound { action_id: String },
}

#[derive(Debug, Default)]
pub struct RtCommandScheduler {
    scheduled: BTreeMap<String, ScheduledRtCommandEntry>,
    due_order: Vec<ScheduledDueKey>,
    due_frontier: usize,
}

impl RtCommandScheduler {
    pub fn schedule(
        &mut self,
        request: RtCommandScheduleRequest,
        now_frame: u64,
        sample_rate: u32,
    ) -> Result<(), RtCommandSchedulerError> {
        self.reclaim_dispatched();
        let validation = validate_schedule_request(
            request.action_id,
            request.origin,
            ScheduleRequestTiming {
                submitted_at_frame: now_frame,
                trigger_frame: request.trigger_frame,
            },
            sample_rate,
        )?;
        let key = validation.action_id.as_str().to_string();
        if self.scheduled.contains_key(&key) {
            return Err(RtCommandSchedulerError::DuplicateAction { action_id: key });
        }

        self.scheduled.insert(
            key.clone(),
            ScheduledRtCommandEntry {
                action: ScheduledRtCommand {
                    action_id: validation.action_id,
                    origin: validation.origin,
                    trigger_frame: validation.timing.trigger_frame,
                    command: request.command,
                },
                state: ScheduledRtCommandState::Pending,
            },
        );
        self.insert_due_key(validation.timing.trigger_frame, key);
        Ok(())
    }

    /// Reclaims metadata for commands already dispatched by the render path.
    ///
    /// This is a non-render/control-side maintenance operation: it may shift the
    /// trigger-time index and retain the id map, so it must not be called from the
    /// audio callback.
    pub fn reclaim_dispatched_metadata(&mut self) -> usize {
        let before = self.scheduled.len();
        if self.due_frontier > 0 {
            self.due_order.drain(..self.due_frontier);
            self.due_frontier = 0;
        }
        self.scheduled
            .retain(|_, entry| entry.state == ScheduledRtCommandState::Pending);
        before.saturating_sub(self.scheduled.len())
    }

    fn reclaim_dispatched(&mut self) {
        self.reclaim_dispatched_metadata();
    }

    fn insert_due_key(&mut self, trigger_frame: u64, action_id: String) {
        let position = self
            .due_order
            .binary_search_by(|probe| {
                (probe.trigger_frame, probe.action_id.as_str())
                    .cmp(&(trigger_frame, action_id.as_str()))
            })
            .unwrap_or_else(|position| position);

        self.due_order.insert(
            position,
            ScheduledDueKey {
                trigger_frame,
                action_id,
            },
        );
    }

    fn remove_due_key(&mut self, trigger_frame: u64, action_id: &str) {
        if let Ok(position) = self.due_order.binary_search_by(|probe| {
            (probe.trigger_frame, probe.action_id.as_str()).cmp(&(trigger_frame, action_id))
        }) {
            self.due_order.remove(position);
            if self.due_frontier > position {
                self.due_frontier -= 1;
            }
        }
    }

    pub fn cancel(
        &mut self,
        action_id: &ScheduledActionId,
    ) -> Result<ScheduledRtCommand, RtCommandSchedulerError> {
        self.reclaim_dispatched();
        self.scheduled
            .remove(action_id.as_str())
            .map(|entry| {
                self.remove_due_key(entry.action.trigger_frame, action_id.as_str());
                entry.action
            })
            .ok_or_else(|| RtCommandSchedulerError::ActionNotFound {
                action_id: action_id.as_str().to_string(),
            })
    }

    pub fn modify_trigger_frame(
        &mut self,
        action_id: &ScheduledActionId,
        new_trigger_frame: u64,
        now_frame: u64,
        sample_rate: u32,
    ) -> Result<(), RtCommandSchedulerError> {
        self.reclaim_dispatched();
        let action_id_key = action_id.as_str().to_string();
        let Some(scheduled) = self.scheduled.get(action_id.as_str()) else {
            return Err(RtCommandSchedulerError::ActionNotFound {
                action_id: action_id_key.clone(),
            });
        };
        let validation = validate_schedule_request(
            scheduled.action.action_id.clone(),
            scheduled.action.origin,
            ScheduleRequestTiming {
                submitted_at_frame: now_frame,
                trigger_frame: new_trigger_frame,
            },
            sample_rate,
        );

        match validation {
            Ok(validation) => {
                let old_trigger_frame = scheduled.action.trigger_frame;
                self.remove_due_key(old_trigger_frame, action_id.as_str());
                let Some(scheduled) = self.scheduled.get_mut(action_id.as_str()) else {
                    return Err(RtCommandSchedulerError::ActionNotFound {
                        action_id: action_id_key,
                    });
                };
                scheduled.action.trigger_frame = validation.timing.trigger_frame;
                self.insert_due_key(validation.timing.trigger_frame, action_id_key);
                Ok(())
            }
            Err(err) => Err(RtCommandSchedulerError::Validation(err)),
        }
    }

    pub fn pop_next_due(&mut self, now_frame: u64) -> Option<RtCommand> {
        while let Some(key) = self.due_order.get(self.due_frontier) {
            if key.trigger_frame > now_frame {
                return None;
            }

            self.due_frontier += 1;
            let Some(entry) = self.scheduled.get_mut(key.action_id.as_str()) else {
                continue;
            };
            if entry.state != ScheduledRtCommandState::Pending
                || entry.action.trigger_frame != key.trigger_frame
            {
                continue;
            }

            entry.state = ScheduledRtCommandState::Dispatched;
            return Some(entry.action.command);
        }

        None
    }

    pub fn drain_due(
        &mut self,
        now_frame: u64,
        max: usize,
        sink: &mut impl FnMut(RtCommand),
    ) -> usize {
        let mut drained = 0;
        while drained < max {
            let Some(command) = self.pop_next_due(now_frame) else {
                break;
            };
            sink(command);
            drained += 1;
        }
        drained
    }

    pub fn len(&self) -> usize {
        self.scheduled
            .values()
            .filter(|entry| entry.state == ScheduledRtCommandState::Pending)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    pub(crate) fn debug_due_frontier_position(&self) -> usize {
        self.due_frontier
    }

    #[cfg(test)]
    pub(crate) fn debug_due_index_len(&self) -> usize {
        self.due_order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::MAX_DRAIN_PER_BLOCK;
    use omm_protocol::SourceId;

    const SAMPLE_RATE: u32 = 48_000;

    fn request(id: &str, origin: ActionOrigin, trigger_frame: u64) -> RtCommandScheduleRequest {
        request_with_command(
            id,
            origin,
            trigger_frame,
            RtCommand::SetChannelEnabled {
                source_id: SourceId::Player,
                enabled: false,
            },
        )
    }

    fn request_with_command(
        id: &str,
        origin: ActionOrigin,
        trigger_frame: u64,
        command: RtCommand,
    ) -> RtCommandScheduleRequest {
        RtCommandScheduleRequest {
            action_id: ScheduledActionId::new(id),
            origin,
            trigger_frame,
            command,
        }
    }

    fn indexed_command(index: u32) -> RtCommand {
        RtCommand::SetMasterGainDb {
            db: -(index as f32),
            ramp_frames: index,
        }
    }

    fn command_index(command: RtCommand) -> u32 {
        match command {
            RtCommand::SetMasterGainDb { ramp_frames, .. } => ramp_frames,
            _ => 0,
        }
    }

    #[test]
    fn planned_commands_before_thirty_seconds_are_rejected() {
        let mut scheduler = RtCommandScheduler::default();
        let err = scheduler
            .schedule(
                request("too-soon", ActionOrigin::PlannedLlm, 1_000),
                0,
                SAMPLE_RATE,
            )
            .expect_err("planned action before now+30s must be rejected");

        assert!(matches!(
            err,
            RtCommandSchedulerError::Validation(
                ScheduleValidationError::PlannedActionTooSoon { .. }
            )
        ));
        assert!(scheduler.is_empty());
    }

    #[test]
    fn immediate_commands_can_be_due_now() {
        let mut scheduler = RtCommandScheduler::default();
        scheduler
            .schedule(
                request("manual-now", ActionOrigin::Manual, 0),
                0,
                SAMPLE_RATE,
            )
            .expect("manual action can be immediate");

        let mut drained = Vec::new();
        let count =
            scheduler.drain_due(0, MAX_DRAIN_PER_BLOCK, &mut |command| drained.push(command));

        assert_eq!(count, 1);
        assert_eq!(drained.len(), 1);
        assert!(scheduler.is_empty());
    }

    #[test]
    fn future_commands_drain_in_engine_time_order() {
        let mut scheduler = RtCommandScheduler::default();
        scheduler
            .schedule(request("z-late", ActionOrigin::Test, 200), 0, SAMPLE_RATE)
            .expect("late action accepted");
        scheduler
            .schedule(request("a-early", ActionOrigin::Test, 100), 0, SAMPLE_RATE)
            .expect("early action accepted");

        let mut drained = 0;
        assert_eq!(
            scheduler.drain_due(99, MAX_DRAIN_PER_BLOCK, &mut |_| drained += 1),
            0
        );
        assert_eq!(
            scheduler.drain_due(100, MAX_DRAIN_PER_BLOCK, &mut |_| drained += 1),
            1
        );
        assert_eq!(drained, 1);
        assert_eq!(scheduler.len(), 1);
        assert_eq!(
            scheduler.drain_due(200, MAX_DRAIN_PER_BLOCK, &mut |_| drained += 1),
            1
        );
        assert_eq!(drained, 2);
        assert!(scheduler.is_empty());
    }

    #[test]
    fn commands_due_in_same_block_drain_by_trigger_then_id() {
        let mut scheduler = RtCommandScheduler::default();
        scheduler
            .schedule(
                request_with_command("z-second", ActionOrigin::Test, 200, indexed_command(2)),
                0,
                SAMPLE_RATE,
            )
            .expect("second action accepted");
        scheduler
            .schedule(
                request_with_command("a-third", ActionOrigin::Test, 200, indexed_command(3)),
                0,
                SAMPLE_RATE,
            )
            .expect("third action accepted");
        scheduler
            .schedule(
                request_with_command("m-first", ActionOrigin::Test, 100, indexed_command(1)),
                0,
                SAMPLE_RATE,
            )
            .expect("first action accepted");

        let mut drained = Vec::new();
        let count = scheduler.drain_due(200, MAX_DRAIN_PER_BLOCK, &mut |command| {
            drained.push(command_index(command))
        });

        assert_eq!(count, 3);
        assert_eq!(drained, vec![1, 3, 2]);
    }

    #[test]
    fn due_drain_is_bounded_and_defers_excess_commands() {
        let mut scheduler = RtCommandScheduler::default();

        for index in 0..=MAX_DRAIN_PER_BLOCK {
            scheduler
                .schedule(
                    request_with_command(
                        &format!("action-{index:03}"),
                        ActionOrigin::Test,
                        0,
                        indexed_command(index as u32),
                    ),
                    0,
                    SAMPLE_RATE,
                )
                .expect("due action accepted");
        }

        let mut first_block = Vec::new();
        assert_eq!(
            scheduler.drain_due(0, MAX_DRAIN_PER_BLOCK, &mut |command| {
                first_block.push(command_index(command))
            }),
            MAX_DRAIN_PER_BLOCK
        );
        assert_eq!(first_block.len(), MAX_DRAIN_PER_BLOCK);
        assert_eq!(scheduler.len(), 1);

        let mut second_block = Vec::new();
        assert_eq!(
            scheduler.drain_due(0, MAX_DRAIN_PER_BLOCK, &mut |command| {
                second_block.push(command_index(command))
            }),
            1
        );
        assert_eq!(second_block, vec![MAX_DRAIN_PER_BLOCK as u32]);
        assert!(scheduler.is_empty());
    }

    #[test]
    fn due_drain_advances_frontier_without_scanning_future_actions() {
        let mut scheduler = RtCommandScheduler::default();
        scheduler
            .schedule(
                request_with_command("due-now", ActionOrigin::Test, 100, indexed_command(1)),
                0,
                SAMPLE_RATE,
            )
            .expect("due action accepted");

        for index in 0..128 {
            scheduler
                .schedule(
                    request_with_command(
                        &format!("future-{index:03}"),
                        ActionOrigin::Test,
                        10_000 + index as u64,
                        indexed_command(100 + index),
                    ),
                    0,
                    SAMPLE_RATE,
                )
                .expect("future action accepted");
        }

        let mut drained = Vec::new();
        assert_eq!(
            scheduler.drain_due(100, MAX_DRAIN_PER_BLOCK, &mut |command| {
                drained.push(command_index(command))
            }),
            1
        );
        assert_eq!(drained, vec![1]);
        assert_eq!(scheduler.debug_due_frontier_position(), 1);
        assert_eq!(scheduler.len(), 128);

        assert_eq!(
            scheduler.drain_due(100, MAX_DRAIN_PER_BLOCK, &mut |_| {}),
            0
        );
        assert_eq!(scheduler.debug_due_frontier_position(), 1);

        scheduler
            .schedule(
                request_with_command(
                    "future-new",
                    ActionOrigin::Test,
                    11_000,
                    indexed_command(300),
                ),
                0,
                SAMPLE_RATE,
            )
            .expect("non-render mutation reclaims dispatched prefix");
        assert_eq!(scheduler.debug_due_frontier_position(), 0);
        assert_eq!(scheduler.debug_due_index_len(), 129);
    }

    #[test]
    fn dispatched_metadata_can_be_reclaimed_off_render_path() {
        let mut scheduler = RtCommandScheduler::default();
        scheduler
            .schedule(request("reclaim-me", ActionOrigin::Test, 0), 0, SAMPLE_RATE)
            .expect("due action accepted");

        assert_eq!(scheduler.drain_due(0, MAX_DRAIN_PER_BLOCK, &mut |_| {}), 1);
        assert_eq!(scheduler.len(), 0);
        assert_eq!(scheduler.debug_due_frontier_position(), 1);
        assert_eq!(scheduler.debug_due_index_len(), 1);

        assert_eq!(scheduler.reclaim_dispatched_metadata(), 1);
        assert_eq!(scheduler.debug_due_frontier_position(), 0);
        assert_eq!(scheduler.debug_due_index_len(), 0);
        assert!(scheduler.is_empty());
    }

    #[test]
    fn duplicate_and_empty_action_ids_are_rejected() {
        let mut scheduler = RtCommandScheduler::default();
        scheduler
            .schedule(request("same", ActionOrigin::Test, 0), 0, SAMPLE_RATE)
            .expect("first action accepted");

        assert!(matches!(
            scheduler.schedule(request("same", ActionOrigin::Test, 10), 0, SAMPLE_RATE),
            Err(RtCommandSchedulerError::DuplicateAction { action_id }) if action_id == "same"
        ));
        assert!(matches!(
            scheduler.schedule(request("", ActionOrigin::Manual, 0), 0, SAMPLE_RATE),
            Err(RtCommandSchedulerError::Validation(
                ScheduleValidationError::EmptyActionId
            ))
        ));
    }

    #[test]
    fn successful_trigger_modification_reindexes_due_order() {
        let mut scheduler = RtCommandScheduler::default();
        let modified_id = ScheduledActionId::new("modified");
        scheduler
            .schedule(
                request_with_command(
                    modified_id.as_str(),
                    ActionOrigin::Test,
                    100,
                    indexed_command(1),
                ),
                0,
                SAMPLE_RATE,
            )
            .expect("action accepted");
        scheduler
            .schedule(
                request_with_command("other", ActionOrigin::Test, 200, indexed_command(2)),
                0,
                SAMPLE_RATE,
            )
            .expect("other action accepted");

        scheduler
            .modify_trigger_frame(&modified_id, 300, 0, SAMPLE_RATE)
            .expect("test-origin action can move later");

        let mut drained = Vec::new();
        assert_eq!(
            scheduler.drain_due(100, MAX_DRAIN_PER_BLOCK, &mut |command| {
                drained.push(command_index(command))
            }),
            0
        );
        assert_eq!(
            scheduler.drain_due(200, MAX_DRAIN_PER_BLOCK, &mut |command| {
                drained.push(command_index(command))
            }),
            1
        );
        assert_eq!(drained, vec![2]);
        assert_eq!(
            scheduler.drain_due(300, MAX_DRAIN_PER_BLOCK, &mut |command| {
                drained.push(command_index(command))
            }),
            1
        );
        assert_eq!(drained, vec![2, 1]);
        assert!(scheduler.is_empty());
    }

    #[test]
    fn cancel_and_modify_paths_report_errors() {
        let mut scheduler = RtCommandScheduler::default();
        let id = ScheduledActionId::new("planned");
        scheduler
            .schedule(
                RtCommandScheduleRequest {
                    action_id: id.clone(),
                    ..request("planned", ActionOrigin::PlannedPi, 1_440_000)
                },
                0,
                SAMPLE_RATE,
            )
            .expect("planned action accepted at now+30s");

        assert!(matches!(
            scheduler.modify_trigger_frame(&id, 10, 0, SAMPLE_RATE),
            Err(RtCommandSchedulerError::Validation(
                ScheduleValidationError::PlannedActionTooSoon { .. }
            ))
        ));
        assert_eq!(scheduler.len(), 1, "failed modification restores action");

        let canceled = scheduler.cancel(&id).expect("existing action canceled");
        assert_eq!(canceled.action_id, id);
        assert!(matches!(
            scheduler.cancel(&ScheduledActionId::new("missing")),
            Err(RtCommandSchedulerError::ActionNotFound { .. })
        ));
        assert!(matches!(
            scheduler.modify_trigger_frame(&ScheduledActionId::new("missing"), 10, 0, SAMPLE_RATE),
            Err(RtCommandSchedulerError::ActionNotFound { .. })
        ));
    }
}
