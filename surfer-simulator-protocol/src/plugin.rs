//! Simulator-independent metadata, value recording, output queue and batching.
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    HierarchyElement, MAX_VALUE_CHANGES_SIZE, PROTOCOL_VERSION, SignalChange, SignalValue, SimId,
    SimulationStatus, SimulationTimestamp, SimulatorToSurferMessage, TimeStep,
    append_value_changes_time_step, encode_simulator_to_surfer_message,
    encode_value_changes_buffer,
};

/// Immutable simulator metadata alongside a backend-owned signal handle map.
/// Handle access and hierarchy traversal remain the backend's responsibility.
pub struct SimulatorData<Handles> {
    pub hierarchy: Vec<HierarchyElement>,
    pub id_to_handle: Arc<Handles>,
    pub simulator_name: String,
    pub simulator_version: String,
    pub time_precision: i32,
}

impl<Handles> SimulatorData<Handles> {
    pub fn simulator_info(&self) -> SimulatorToSurferMessage {
        SimulatorToSurferMessage::SimulatorInfo {
            protocol_version: PROTOCOL_VERSION,
            name: self.simulator_name.clone(),
            version: self.simulator_version.clone(),
        }
    }

    pub fn hierarchy_message(&self) -> SimulatorToSurferMessage {
        SimulatorToSurferMessage::Hierarchy {
            hierarchy: self.hierarchy.clone(),
            exponent: self.time_precision,
        }
    }
}

/// Recorded changes in callback order, grouped by consecutive equal timestamps.
/// Multiple changes to the same signal in a timestep are preserved.
#[derive(Default)]
pub struct ValueChangeBuffer {
    time_steps: Vec<TimeStep>,
}

impl ValueChangeBuffer {
    /// Preserve a pause/completion timestamp even when no signals changed.
    pub fn ensure_time_step(&mut self, timestamp: SimulationTimestamp) {
        if self
            .time_steps
            .last()
            .is_none_or(|step| step.timestamp != timestamp.ticks())
        {
            self.time_steps.push(TimeStep {
                timestamp: timestamp.ticks(),
                changes: Vec::new(),
            });
        }
    }

    /// Register an already converted value; simulator API calls stay in the plugin.
    pub fn record(&mut self, timestamp: SimulationTimestamp, id: SimId, value: SignalValue) {
        self.ensure_time_step(timestamp);
        self.time_steps
            .last_mut()
            .expect("timestamp was just ensured")
            .changes
            .push(SignalChange { id, value });
    }

    /// Remove pending values for untracked signals and discard empty timesteps.
    pub fn remove_signals(&mut self, ids: &HashSet<SimId>) {
        for step in &mut self.time_steps {
            step.changes.retain(|change| !ids.contains(&change.id));
        }
        self.time_steps.retain(|step| !step.changes.is_empty());
    }

    /// Drain before encoding or sending, so callers can release their lock first.
    pub fn take(&mut self) -> Vec<TimeStep> {
        std::mem::take(&mut self.time_steps)
    }
}

/// Queue complete logical messages on the simulator thread. The connection
/// task must send each message with `transport::write_message`.
pub struct ClientConnection {
    outgoing: UnboundedSender<Vec<u8>>,
}

impl ClientConnection {
    pub fn new(outgoing: UnboundedSender<Vec<u8>>) -> Self {
        Self { outgoing }
    }

    pub fn send(&self, message: SimulatorToSurferMessage) {
        if let Ok(encoded) = encode_simulator_to_surfer_message(&message) {
            self.send_encoded(encoded);
        }
    }

    pub fn send_encoded(&self, encoded: Vec<u8>) {
        let _ = self.outgoing.send(encoded);
    }

    /// Emit bounded target-size batches. A single large timestep is allowed:
    /// transport chunking handles it without splitting its timestamp semantics.
    /// Always emit terminal status, including when there are no changes.
    pub fn send_value_changes(&self, time_steps: Vec<TimeStep>, status: SimulationStatus) {
        encode_value_change_batches(time_steps, status, |encoded| self.send_encoded(encoded));
    }
}

pub fn encode_value_change_batches(
    time_steps: impl IntoIterator<Item = TimeStep>,
    status: SimulationStatus,
    mut emit: impl FnMut(Vec<u8>),
) {
    let mut buffer = Vec::new();
    for time_step in time_steps {
        let mut next = Vec::new();
        append_value_changes_time_step(&mut next, &time_step);
        if !buffer.is_empty() && buffer.len() + next.len() > MAX_VALUE_CHANGES_SIZE {
            emit(encode_value_changes_buffer(
                &buffer,
                SimulationStatus::Running,
            ));
            buffer.clear();
        }
        buffer.extend_from_slice(&next);
    }
    if !buffer.is_empty() || status != SimulationStatus::Running {
        emit(encode_value_changes_buffer(&buffer, status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SignalChange, SignalValue, decode_simulator_to_surfer_message};

    #[test]
    fn recording_preserves_callback_order_and_repeated_signal_changes() {
        let mut buffer = ValueChangeBuffer::default();
        for (time, id, value) in [(3, 1, b'0'), (3, 1, b'1'), (3, 2, b'x'), (4, 1, b'0')] {
            buffer.record(
                SimulationTimestamp::from_ticks(time),
                id,
                SignalValue::VCDValue(vec![value]),
            );
        }
        let steps = buffer.take();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].timestamp, 3);
        assert_eq!(steps[1].timestamp, 4);
        let changes: Vec<_> = steps[0]
            .changes
            .iter()
            .map(|change| {
                let SignalValue::VCDValue(value) = &change.value;
                (change.id, value[0])
            })
            .collect();
        assert_eq!(changes, [(1, b'0'), (1, b'1'), (2, b'x')]);
        assert!(buffer.take().is_empty());
    }

    #[test]
    fn untracking_purges_pending_values_and_preserves_other_signals() {
        let mut buffer = ValueChangeBuffer::default();
        for (time, id) in [(3, 1), (3, 2), (4, 1)] {
            buffer.record(
                SimulationTimestamp::from_ticks(time),
                id,
                SignalValue::VCDValue(b"r1.0".to_vec()),
            );
        }
        buffer.remove_signals(&HashSet::from([1]));
        let steps = buffer.take();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].timestamp, 3);
        assert_eq!(steps[0].changes.len(), 1);
        assert_eq!(steps[0].changes[0].id, 2);

        // Even with no tracked values, pause/completion must convey the time.
        buffer.ensure_time_step(SimulationTimestamp::from_ticks(8));
        buffer.ensure_time_step(SimulationTimestamp::from_ticks(8));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        ClientConnection::new(tx).send_value_changes(buffer.take(), SimulationStatus::Paused);
        let SimulatorToSurferMessage::ValueChanges { time_steps, status } =
            decode_simulator_to_surfer_message(&rx.try_recv().unwrap()).unwrap()
        else {
            panic!("wrong message type")
        };
        assert_eq!(status, SimulationStatus::Paused);
        assert_eq!(time_steps.len(), 1);
        assert_eq!(time_steps[0].timestamp, 8);
        assert!(time_steps[0].changes.is_empty());
    }

    #[test]
    fn batching_preserves_timesteps_and_only_finishes_last_batch() {
        let steps = (0..3).map(|timestamp| TimeStep {
            timestamp,
            changes: vec![SignalChange {
                id: 1,
                value: SignalValue::VCDValue(vec![b'1'; MAX_VALUE_CHANGES_SIZE]),
            }],
        });
        let mut messages = Vec::new();
        encode_value_change_batches(steps, SimulationStatus::Done, |encoded| {
            messages.push(encoded)
        });
        assert_eq!(messages.len(), 3);
        for (index, encoded) in messages.iter().enumerate() {
            let SimulatorToSurferMessage::ValueChanges { time_steps, status } =
                decode_simulator_to_surfer_message(encoded).unwrap()
            else {
                panic!("wrong message type")
            };
            assert_eq!(time_steps.len(), 1);
            assert_eq!(time_steps[0].timestamp, index as u64);
            assert_eq!(
                status,
                if index == 2 {
                    SimulationStatus::Done
                } else {
                    SimulationStatus::Running
                }
            );
        }
    }

    #[test]
    fn empty_terminal_updates_reach_the_queue() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let connection = ClientConnection::new(tx);
        for status in [SimulationStatus::Paused, SimulationStatus::Done] {
            connection.send_value_changes(Vec::new(), status);
            let SimulatorToSurferMessage::ValueChanges {
                time_steps,
                status: decoded_status,
            } = decode_simulator_to_surfer_message(&rx.try_recv().unwrap()).unwrap()
            else {
                panic!("wrong message type")
            };
            assert!(time_steps.is_empty());
            assert_eq!(decoded_status, status);
        }
        connection.send_value_changes(Vec::new(), SimulationStatus::Running);
        assert!(rx.try_recv().is_err());
    }
}
