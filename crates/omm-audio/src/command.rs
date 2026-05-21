use omm_protocol::params::SourceId;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

pub const RT_QUEUE_CAPACITY: usize = 1024;
pub const MAX_DRAIN_PER_BLOCK: usize = 64;
pub const RT_SOURCE_INSTANCE_ID_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtSourceInstanceId {
    bytes: [u8; RT_SOURCE_INSTANCE_ID_CAPACITY],
    len: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RtSourceInstanceIdError {
    #[error("RT source instance id must not be empty")]
    Empty,
    #[error("RT source instance id exceeds {RT_SOURCE_INSTANCE_ID_CAPACITY} bytes")]
    TooLong,
    #[error("RT source instance id contains unsupported characters")]
    InvalidCharacter,
}

impl RtSourceInstanceId {
    pub fn new(value: &str) -> Self {
        Self::try_new(value).expect("valid RT source instance id")
    }

    pub fn try_new(value: &str) -> Result<Self, RtSourceInstanceIdError> {
        if value.is_empty() {
            return Err(RtSourceInstanceIdError::Empty);
        }
        if value.len() > RT_SOURCE_INSTANCE_ID_CAPACITY {
            return Err(RtSourceInstanceIdError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_' | b'.'))
        {
            return Err(RtSourceInstanceIdError::InvalidCharacter);
        }

        let mut bytes = [0_u8; RT_SOURCE_INSTANCE_ID_CAPACITY];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Ok(Self {
            bytes,
            len: value.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("RT source instance id is always valid UTF-8")
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RtCommand {
    SetMasterGainDb {
        db: f32,
        ramp_frames: u32,
    },
    SetMasterPan {
        pan: f32,
        ramp_frames: u32,
    },
    SetMasterLowpassHz {
        hz: f32,
    },
    SetMasterHighpassHz {
        hz: f32,
    },
    SetChannelGainDb {
        source_id: SourceId,
        db: f32,
        ramp_frames: u32,
    },
    SetChannelPan {
        source_id: SourceId,
        pan: f32,
        ramp_frames: u32,
    },
    SetChannelLowpassHz {
        source_id: SourceId,
        hz: f32,
    },
    SetChannelHighpassHz {
        source_id: SourceId,
        hz: f32,
    },
    SetChannelEnabled {
        source_id: SourceId,
        enabled: bool,
    },
    SetSourceInstanceGainDb {
        source_instance_id: RtSourceInstanceId,
        db: f32,
        ramp_frames: u32,
    },
    SetSourceInstancePan {
        source_instance_id: RtSourceInstanceId,
        pan: f32,
        ramp_frames: u32,
    },
    SetSourceInstanceHighpassHz {
        source_instance_id: RtSourceInstanceId,
        hz: f32,
    },
    SetSourceInstanceLowpassHz {
        source_instance_id: RtSourceInstanceId,
        hz: f32,
    },
    SetSourceInstanceEq {
        source_instance_id: RtSourceInstanceId,
        low_db: f32,
        mid_db: f32,
        high_db: f32,
        ramp_frames: u32,
    },
    SetSourceInstanceReverbSendDb {
        source_instance_id: RtSourceInstanceId,
        send_db: f32,
        ramp_frames: u32,
    },
    SetSourceInstancePlaybackRate {
        source_instance_id: RtSourceInstanceId,
        rate: f32,
        ramp_frames: u32,
    },
    SetSourceInstanceReverse {
        source_instance_id: RtSourceInstanceId,
        reverse: bool,
    },
}

pub struct CommandQueue {
    producer: HeapProd<RtCommand>,
}

pub struct CommandReceiver {
    consumer: HeapCons<RtCommand>,
}

#[derive(Debug, thiserror::Error)]
#[error("RT command queue full")]
pub struct QueueFull;

pub fn new_command_channel() -> (CommandQueue, CommandReceiver) {
    let queue = HeapRb::<RtCommand>::new(RT_QUEUE_CAPACITY);
    let (producer, consumer) = queue.split();

    (CommandQueue { producer }, CommandReceiver { consumer })
}

impl CommandQueue {
    pub fn enqueue(&mut self, cmd: RtCommand) -> Result<(), QueueFull> {
        self.producer.try_push(cmd).map_err(|_| QueueFull)
    }
}

impl CommandReceiver {
    pub fn drain(&mut self, sink: &mut impl FnMut(RtCommand), max: usize) -> usize {
        let mut count = 0;

        while count < max {
            let Some(cmd) = self.consumer.try_pop() else {
                break;
            };

            sink(cmd);
            count += 1;
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn assert_copy<T: Copy>() {}

    fn command(index: u32) -> RtCommand {
        RtCommand::SetMasterGainDb {
            db: -(index as f32),
            ramp_frames: index,
        }
    }

    fn command_index(cmd: RtCommand) -> u32 {
        match cmd {
            RtCommand::SetMasterGainDb { ramp_frames, .. } => ramp_frames,
            _ => 0,
        }
    }

    #[test]
    fn full_capacity_accepts_1024_and_rejects_1025th() {
        let (mut queue, _receiver) = new_command_channel();

        for index in 0..RT_QUEUE_CAPACITY {
            assert!(queue.enqueue(command(index as u32)).is_ok());
        }

        assert!(matches!(
            queue.enqueue(command(RT_QUEUE_CAPACITY as u32)),
            Err(QueueFull)
        ));
    }

    #[test]
    fn drain_stops_at_requested_max() {
        let (mut queue, mut receiver) = new_command_channel();

        for index in 0..10 {
            assert!(queue.enqueue(command(index)).is_ok());
        }

        let mut drained = Vec::new();
        let count = receiver.drain(&mut |cmd| drained.push(command_index(cmd)), 3);

        assert_eq!(count, 3);
        assert_eq!(drained, [0, 1, 2]);

        let count = receiver.drain(
            &mut |cmd| drained.push(command_index(cmd)),
            MAX_DRAIN_PER_BLOCK,
        );

        assert_eq!(count, 7);
        assert_eq!(drained, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn full_queue_drains_in_sixteen_bounded_blocks() {
        let (mut queue, mut receiver) = new_command_channel();

        for index in 0..RT_QUEUE_CAPACITY {
            assert!(queue.enqueue(command(index as u32)).is_ok());
        }

        let mut drained = Vec::new();
        let mut total = 0;

        for _ in 0..16 {
            total += receiver.drain(
                &mut |cmd| drained.push(command_index(cmd)),
                MAX_DRAIN_PER_BLOCK,
            );
        }

        assert_eq!(total, RT_QUEUE_CAPACITY);
        assert_eq!(drained.len(), RT_QUEUE_CAPACITY);
        assert_eq!(receiver.drain(&mut |_| {}, MAX_DRAIN_PER_BLOCK), 0);

        for (index, value) in drained.into_iter().enumerate() {
            assert_eq!(value, index as u32);
        }
    }

    #[test]
    fn concurrent_spsc_transfers_each_command_once() {
        let (mut queue, mut receiver) = new_command_channel();
        let total = 1000;

        let producer = thread::spawn(move || {
            for index in 0..total {
                loop {
                    if queue.enqueue(command(index)).is_ok() {
                        break;
                    }

                    thread::yield_now();
                }
            }
        });

        let consumer = thread::spawn(move || {
            let mut drained = Vec::with_capacity(total as usize);

            while drained.len() < total as usize {
                let count = receiver.drain(
                    &mut |cmd| drained.push(command_index(cmd)),
                    MAX_DRAIN_PER_BLOCK,
                );

                if count == 0 {
                    thread::yield_now();
                }
            }

            drained
        });

        producer.join().unwrap();
        let mut drained = consumer.join().unwrap();

        drained.sort_unstable();
        assert_eq!(drained.len(), total as usize);

        for (index, value) in drained.into_iter().enumerate() {
            assert_eq!(value, index as u32);
        }
    }

    #[test]
    fn rt_command_defines_expected_variants() {
        assert_copy::<RtCommand>();

        fn assert_variant_is_covered(command: RtCommand) {
            match command {
                RtCommand::SetMasterGainDb { .. }
                | RtCommand::SetMasterPan { .. }
                | RtCommand::SetMasterLowpassHz { .. }
                | RtCommand::SetMasterHighpassHz { .. }
                | RtCommand::SetChannelGainDb { .. }
                | RtCommand::SetChannelPan { .. }
                | RtCommand::SetChannelLowpassHz { .. }
                | RtCommand::SetChannelHighpassHz { .. }
                | RtCommand::SetChannelEnabled { .. }
                | RtCommand::SetSourceInstanceGainDb { .. }
                | RtCommand::SetSourceInstancePan { .. }
                | RtCommand::SetSourceInstanceHighpassHz { .. }
                | RtCommand::SetSourceInstanceLowpassHz { .. }
                | RtCommand::SetSourceInstanceEq { .. }
                | RtCommand::SetSourceInstanceReverbSendDb { .. }
                | RtCommand::SetSourceInstancePlaybackRate { .. }
                | RtCommand::SetSourceInstanceReverse { .. } => {}
            }
        }

        let commands = [
            RtCommand::SetMasterGainDb {
                db: -6.0,
                ramp_frames: 4800,
            },
            RtCommand::SetMasterPan {
                pan: 0.5,
                ramp_frames: 4800,
            },
            RtCommand::SetMasterLowpassHz { hz: 18_000.0 },
            RtCommand::SetMasterHighpassHz { hz: 20.0 },
            RtCommand::SetChannelGainDb {
                source_id: SourceId::System,
                db: -3.0,
                ramp_frames: 9600,
            },
            RtCommand::SetChannelPan {
                source_id: SourceId::Mic,
                pan: -0.25,
                ramp_frames: 9600,
            },
            RtCommand::SetChannelLowpassHz {
                source_id: SourceId::Player,
                hz: 12_000.0,
            },
            RtCommand::SetChannelHighpassHz {
                source_id: SourceId::Glicol,
                hz: 60.0,
            },
            RtCommand::SetChannelEnabled {
                source_id: SourceId::System,
                enabled: true,
            },
            RtCommand::SetSourceInstanceGainDb {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                db: -6.0,
                ramp_frames: 4800,
            },
            RtCommand::SetSourceInstancePan {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                pan: 0.25,
                ramp_frames: 4800,
            },
            RtCommand::SetSourceInstanceHighpassHz {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                hz: 80.0,
            },
            RtCommand::SetSourceInstanceLowpassHz {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                hz: 12_000.0,
            },
            RtCommand::SetSourceInstanceEq {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                low_db: 3.0,
                mid_db: -2.0,
                high_db: 1.0,
                ramp_frames: 4800,
            },
            RtCommand::SetSourceInstanceReverbSendDb {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                send_db: -12.0,
                ramp_frames: 4800,
            },
            RtCommand::SetSourceInstancePlaybackRate {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                rate: 1.5,
                ramp_frames: 4800,
            },
            RtCommand::SetSourceInstanceReverse {
                source_instance_id: RtSourceInstanceId::new("file:loop"),
                reverse: true,
            },
        ];

        for command in commands {
            assert_variant_is_covered(command);
        }
    }

    #[test]
    fn rt_source_instance_id_is_fixed_capacity() {
        let id = RtSourceInstanceId::new("file:loop-01");

        assert_eq!(id.as_str(), "file:loop-01");
        assert!(RtSourceInstanceId::try_new("").is_err());
        assert!(RtSourceInstanceId::try_new("file/loop").is_err());
        assert!(
            RtSourceInstanceId::try_new(&"x".repeat(RT_SOURCE_INSTANCE_ID_CAPACITY + 1)).is_err()
        );
    }
}
