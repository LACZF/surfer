#![deny(unused_crate_dependencies)]

use std::error::Error;
use std::fmt::{Display, Formatter};

use prost::Message;
use prost::encoding::encode_varint;
pub use proto::{ScopeType, SimulationStatus, VarDirection, VarIndex, VarType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SignalEncoding {
    String,
    Real,
    BitVector(u32),
}

/// Identifier assigned to a variable by the simulator.
pub type SimId = u32;

pub const DEFAULT_SIMULATOR_PORT: u16 = 6967;

/// Maximum encoded simulator message size accepted by the transport.
pub const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;
/// Space reserved for protobuf and framing overhead.
pub const FRAME_SIZE_MARGIN: usize = 64 * 1024;
/// Maximum encoded `ValueChanges` payload before starting another frame.
pub const MAX_VALUE_CHANGES_SIZE: usize = MAX_FRAME_SIZE - FRAME_SIZE_MARGIN;

pub mod proto {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/generated/surfer.simulator.protocol.v1.rs"
    ));
}

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug)]
pub enum ProtocolError {
    Encode(prost::EncodeError),
    Decode(prost::DecodeError),
    MissingField(&'static str),
    InvalidEnum { name: &'static str, value: i32 },
    InvalidValue(&'static str),
}

impl Display for ProtocolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Encode(err) => write!(f, "failed to encode protobuf message: {err}"),
            ProtocolError::Decode(err) => write!(f, "failed to decode protobuf message: {err}"),
            ProtocolError::MissingField(name) => write!(f, "missing required field: {name}"),
            ProtocolError::InvalidEnum { name, value } => {
                write!(f, "invalid enum value for {name}: {value}")
            }
            ProtocolError::InvalidValue(name) => write!(f, "invalid value for field: {name}"),
        }
    }
}

impl Error for ProtocolError {}

impl From<prost::EncodeError> for ProtocolError {
    fn from(value: prost::EncodeError) -> Self {
        ProtocolError::Encode(value)
    }
}

impl From<prost::DecodeError> for ProtocolError {
    fn from(value: prost::DecodeError) -> Self {
        ProtocolError::Decode(value)
    }
}

pub fn encode_surfer_to_simulator_message(
    message: &SurferToSimulatorMessage,
) -> Result<Vec<u8>, ProtocolError> {
    let kind = match message {
        SurferToSimulatorMessage::RequestSimulatorInfo => {
            proto::surfer_to_simulator::Kind::RequestSimulatorInfo(proto::Empty {})
        }
        SurferToSimulatorMessage::RunSimulation { time } => {
            proto::surfer_to_simulator::Kind::RunSimulation(proto::RunSimulation { time: *time })
        }
        SurferToSimulatorMessage::PauseSimulation => {
            proto::surfer_to_simulator::Kind::PauseSimulation(proto::Empty {})
        }
        SurferToSimulatorMessage::RequestHierarchy => {
            proto::surfer_to_simulator::Kind::RequestHierarchy(proto::Empty {})
        }
        SurferToSimulatorMessage::TrackVariableChanges { ids } => {
            proto::surfer_to_simulator::Kind::TrackVariableChanges(proto::TrackVariableChanges {
                ids: ids.clone(),
            })
        }
        SurferToSimulatorMessage::UntrackVariableChanges { ids } => {
            proto::surfer_to_simulator::Kind::UntrackVariableChanges(
                proto::UntrackVariableChanges { ids: ids.clone() },
            )
        }
        SurferToSimulatorMessage::Shutdown => {
            proto::surfer_to_simulator::Kind::Shutdown(proto::Empty {})
        }
    };
    let pb = proto::SurferToSimulator { kind: Some(kind) };
    Ok(pb.encode_to_vec())
}

pub fn decode_surfer_to_simulator_message(
    data: &[u8],
) -> Result<SurferToSimulatorMessage, ProtocolError> {
    let pb = proto::SurferToSimulator::decode(data)?;
    let kind = required_field(pb.kind, "SurferToSimulator.kind")?;
    match kind {
        proto::surfer_to_simulator::Kind::RequestSimulatorInfo(_) => {
            Ok(SurferToSimulatorMessage::RequestSimulatorInfo)
        }
        proto::surfer_to_simulator::Kind::RunSimulation(run) => {
            Ok(SurferToSimulatorMessage::RunSimulation { time: run.time })
        }
        proto::surfer_to_simulator::Kind::PauseSimulation(_) => {
            Ok(SurferToSimulatorMessage::PauseSimulation)
        }
        proto::surfer_to_simulator::Kind::RequestHierarchy(_) => {
            Ok(SurferToSimulatorMessage::RequestHierarchy)
        }
        proto::surfer_to_simulator::Kind::TrackVariableChanges(track) => {
            Ok(SurferToSimulatorMessage::TrackVariableChanges { ids: track.ids })
        }
        proto::surfer_to_simulator::Kind::UntrackVariableChanges(untrack) => {
            Ok(SurferToSimulatorMessage::UntrackVariableChanges { ids: untrack.ids })
        }
        proto::surfer_to_simulator::Kind::Shutdown(_) => Ok(SurferToSimulatorMessage::Shutdown),
    }
}

pub fn encode_simulator_to_surfer_message(
    message: &SimulatorToSurferMessage,
) -> Result<Vec<u8>, ProtocolError> {
    let kind = match message {
        SimulatorToSurferMessage::SimulatorInfo {
            protocol_version,
            name,
            version,
        } => proto::simulator_to_surfer::Kind::SimulatorInfo(proto::SimulatorInfo {
            protocol_version: *protocol_version,
            name: name.clone(),
            version: version.clone(),
        }),
        SimulatorToSurferMessage::Hierarchy {
            hierarchy,
            exponent,
        } => proto::simulator_to_surfer::Kind::Hierarchy(proto::Hierarchy {
            hierarchy: hierarchy.iter().map(to_hierarchy_element_pb).collect(),
            exponent: Some(*exponent),
        }),
        SimulatorToSurferMessage::Acknowledge { success } => {
            proto::simulator_to_surfer::Kind::Acknowledge(proto::Acknowledge { success: *success })
        }
        SimulatorToSurferMessage::ValueChanges { time_steps, status } => {
            proto::simulator_to_surfer::Kind::ValueChanges(proto::ValueChanges {
                time_steps: time_steps.iter().map(to_time_step_pb).collect(),
                status: *status as i32,
            })
        }
        SimulatorToSurferMessage::CurrentSimulationTime { time } => {
            proto::simulator_to_surfer::Kind::CurrentSimulationTime(proto::CurrentSimulationTime {
                time: *time,
            })
        }
    };
    let pb = proto::SimulatorToSurfer { kind: Some(kind) };
    Ok(pb.encode_to_vec())
}

/// Append one timestep to the wire payload of a `ValueChanges` message.
///
/// The returned bytes are the `ValueChanges` protobuf message without its
/// outer `SimulatorToSurfer` envelope. This allows large change streams to be
/// encoded incrementally without re-encoding all preceding timesteps.
pub fn append_value_changes_time_step(buffer: &mut Vec<u8>, time_step: &TimeStep) {
    let encoded_time_step = to_time_step_pb(time_step).encode_to_vec();
    encode_varint(0x0a, buffer);
    encode_varint(encoded_time_step.len() as u64, buffer);
    buffer.extend_from_slice(&encoded_time_step);
}

/// Wrap an incrementally encoded `ValueChanges` payload in its protocol envelope.
#[must_use]
pub fn encode_value_changes_buffer(buffer: &[u8], status: SimulationStatus) -> Vec<u8> {
    let mut value_changes = buffer.to_vec();
    encode_varint(0x10, &mut value_changes);
    encode_varint(status as u64, &mut value_changes);

    let mut encoded = Vec::with_capacity(value_changes.len() + 8);
    encode_varint(0x22, &mut encoded);
    encode_varint(value_changes.len() as u64, &mut encoded);
    encoded.extend_from_slice(&value_changes);
    encoded
}

pub fn decode_simulator_to_surfer_message(
    data: &[u8],
) -> Result<SimulatorToSurferMessage, ProtocolError> {
    let pb = proto::SimulatorToSurfer::decode(data)?;
    let kind = required_field(pb.kind, "SimulatorToSurfer.kind")?;
    match kind {
        proto::simulator_to_surfer::Kind::SimulatorInfo(info) => {
            Ok(SimulatorToSurferMessage::SimulatorInfo {
                protocol_version: info.protocol_version,
                name: info.name,
                version: info.version,
            })
        }
        proto::simulator_to_surfer::Kind::Hierarchy(hierarchy) => {
            Ok(SimulatorToSurferMessage::Hierarchy {
                hierarchy: hierarchy
                    .hierarchy
                    .into_iter()
                    .map(from_hierarchy_element_pb)
                    .collect::<Result<Vec<_>, _>>()?,
                exponent: required_field(hierarchy.exponent, "Hierarchy.exponent")?,
            })
        }
        proto::simulator_to_surfer::Kind::Acknowledge(ack) => {
            Ok(SimulatorToSurferMessage::Acknowledge {
                success: ack.success,
            })
        }
        proto::simulator_to_surfer::Kind::ValueChanges(changes) => {
            let status = proto::SimulationStatus::try_from(changes.status).map_err(|_| {
                ProtocolError::InvalidEnum {
                    name: "SimulationStatus",
                    value: changes.status,
                }
            })?;
            if status == SimulationStatus::Unspecified {
                return Err(ProtocolError::InvalidValue("ValueChanges.status"));
            }
            Ok(SimulatorToSurferMessage::ValueChanges {
                time_steps: changes
                    .time_steps
                    .into_iter()
                    .map(from_time_step_pb)
                    .collect::<Result<Vec<_>, _>>()?,
                status,
            })
        }
        proto::simulator_to_surfer::Kind::CurrentSimulationTime(current) => {
            Ok(SimulatorToSurferMessage::CurrentSimulationTime { time: current.time })
        }
    }
}

/// Messages sent by the simulator to Surfer
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SimulatorToSurferMessage {
    /// Basic simulator metadata.
    SimulatorInfo {
        protocol_version: u32,
        name: String,
        version: String,
    },
    /// Full hierarchy and simulation time scale.
    Hierarchy {
        hierarchy: Vec<HierarchyElement>,
        exponent: i32,
    },
    /// Acknowledgement for command-like messages.
    ///
    /// `true` indicates success, `false` indicates failure.
    Acknowledge { success: bool },
    /// Value changes grouped by timesteps and a completion flag.
    ValueChanges {
        time_steps: Vec<TimeStep>,
        status: SimulationStatus,
    },
    /// Current simulation time in simulation time units.
    ///
    ///  This is used to update the user interface.
    /// It is recommended that it is sent based on wall time (e.g. every 500ms)
    /// rather than every time the simulation time changes, to avoid overwhelming the UI with updates.
    CurrentSimulationTime { time: u64 },
}

/// Messages sent by Surfer to the simulator
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SurferToSimulatorMessage {
    /// Request simulator metadata.
    RequestSimulatorInfo,
    /// Start or continue simulation.
    ///
    /// `Some(t)` means advance simulation by `t` time units.
    /// `None` means run until completion.
    RunSimulation { time: Option<u64> },
    /// Pause simulation.
    PauseSimulation,
    /// Request hierarchy and time scale.
    RequestHierarchy,
    /// Start reporting value changes for the variables identified by hierarchy ids.
    TrackVariableChanges { ids: Vec<SimId> },
    /// Stop reporting value changes for the variables identified by hierarchy ids.
    UntrackVariableChanges { ids: Vec<SimId> },
    /// Shutdown simulator and close connection.
    Shutdown,
}

/// One hierarchy operation.
///
/// The hierarchy stream is interpreted sequentially and should form a balanced
/// push/pop scope structure.
///
/// ## How to produce a valid `Vec<HierarchyElement>`
///
/// Emit elements in a depth-first traversal order using this pattern:
/// 1. Emit [`HierarchyElement::PushScope`] when entering a scope.
/// 2. Emit [`HierarchyElement::AddVar`] for variables that belong to that scope.
/// 3. Recurse into child scopes and repeat.
/// 4. Emit [`HierarchyElement::PopScope`] when leaving the scope.
///
/// Practical rules:
/// - The stream must be balanced: every `PushScope` must have exactly one later
///   matching `PopScope`.
/// - `AddVar` applies to the current top-of-stack scope (the most recent unclosed
///   `PushScope`).
/// - Do not emit `PopScope` when no scope is active.
/// - At end-of-stream, all opened scopes must be closed.
///
/// Pseudocode:
/// ```text
/// visit_scope(scope):
///   emit PushScope(scope)
///   for var in scope.variables:
///     emit AddVar(var)
///   for child in scope.children:
///     visit_scope(child)
///   emit PopScope
/// ```

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum HierarchyElement {
    /// Enter a new scope.
    PushScope {
        /// Scope name as it should appear in Surfer.
        name: String,
        /// Scope kind (module, architecture, block, ...).
        scope_type: ScopeType,
    },
    /// Exit the current scope.
    PopScope,
    /// Declare a variable in the current scope.
    AddVar {
        /// Unique variable id used in tracking and value change messages.
        id: SimId,
        /// Variable name.
        name: String,
        /// Index for arrays or multi-dimensional variables, or `None` for scalars.
        index: Option<VarIndex>,
        /// Type name for display/translation.
        type_name: String,
        /// Variable category.
        var_type: VarType,
        /// Value encoding (VCD-compatible).
        signal_encoding: SignalEncoding,
        /// Optional direction metadata.
        var_direction: Option<VarDirection>,
        /// Optional enum mapping from option names to display values.
        ///
        /// Used for VHDL enumerated types and SystemVerilog enums.
        enum_map: Option<Vec<(String, String)>>,
        /// Whether this variable is a parameter or generic.
        is_parameter: bool,
    },
}

/// All recorded value changes at a certain timestep
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeStep {
    /// Timestamp in simulation time units.
    pub timestamp: u64,
    /// Signal changes observed at this timestamp.
    pub changes: Vec<SignalChange>,
}

/// A change for a single value
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignalChange {
    /// Variable id from hierarchy.
    pub id: SimId,
    /// Encoded value as it would appear in a VCD file, without the variable id.
    pub value: SignalValue,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SignalValue {
    /// Value encoded as it would appear in a VCD file, without the timestamp or variable id.
    VCDValue(Vec<u8>),
    /// Value encoded as a real number.
    Real(f64),
    /// Value encoded as it would appear in an FST file.
    FSTValue(Vec<u8>),
}

fn to_hierarchy_element_pb(element: &HierarchyElement) -> proto::HierarchyElement {
    let kind = match element {
        HierarchyElement::PushScope { name, scope_type } => {
            proto::hierarchy_element::Kind::PushScope(proto::PushScope {
                name: name.clone(),
                scope_type: *scope_type as i32,
            })
        }
        HierarchyElement::PopScope => proto::hierarchy_element::Kind::PopScope(proto::Empty {}),
        HierarchyElement::AddVar {
            id,
            name,
            index,
            type_name,
            var_type,
            signal_encoding,
            var_direction,
            enum_map,
            is_parameter,
        } => proto::hierarchy_element::Kind::AddVar(proto::AddVar {
            id: *id,
            name: name.clone(),
            index: *index,
            type_name: type_name.clone(),
            var_type: *var_type as i32,
            signal_encoding: Some(to_signal_encoding_pb(*signal_encoding)),
            var_direction: var_direction
                .as_ref()
                .copied()
                .map(|direction| direction as i32),
            enum_map: enum_map.as_ref().map(|entries| proto::EnumMap {
                entries: entries
                    .iter()
                    .map(|(key, value)| proto::EnumMapEntry {
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .collect(),
            }),
            is_parameter: *is_parameter,
        }),
    };

    proto::HierarchyElement { kind: Some(kind) }
}

fn from_hierarchy_element_pb(
    element: proto::HierarchyElement,
) -> Result<HierarchyElement, ProtocolError> {
    let kind = required_field(element.kind, "HierarchyElement.kind")?;

    match kind {
        proto::hierarchy_element::Kind::PushScope(push_scope) => {
            let scope_type = from_scope_type_pb(push_scope.scope_type)?;
            Ok(HierarchyElement::PushScope {
                name: push_scope.name,
                scope_type,
            })
        }
        proto::hierarchy_element::Kind::PopScope(_) => Ok(HierarchyElement::PopScope),
        proto::hierarchy_element::Kind::AddVar(add_var) => {
            let var_type = from_var_type_pb(add_var.var_type)?;
            let signal_encoding = from_signal_encoding_pb(required_field(
                add_var.signal_encoding,
                "AddVar.signal_encoding",
            )?)?;
            Ok(HierarchyElement::AddVar {
                id: add_var.id,
                name: add_var.name,
                index: add_var.index,
                type_name: add_var.type_name,
                var_type,
                signal_encoding,
                var_direction: add_var
                    .var_direction
                    .map(from_var_direction_pb)
                    .transpose()?,
                enum_map: add_var.enum_map.map(|enum_map| {
                    enum_map
                        .entries
                        .into_iter()
                        .map(|entry| (entry.key, entry.value))
                        .collect()
                }),
                is_parameter: add_var.is_parameter,
            })
        }
    }
}

fn from_scope_type_pb(scope_type: i32) -> Result<ScopeType, ProtocolError> {
    let scope_type =
        proto::ScopeType::try_from(scope_type).map_err(|_| ProtocolError::InvalidEnum {
            name: "ScopeType",
            value: scope_type,
        })?;

    if scope_type == ScopeType::Unspecified {
        return Err(ProtocolError::InvalidValue("PushScope.scope_type"));
    }

    Ok(scope_type)
}

fn from_var_type_pb(var_type: i32) -> Result<VarType, ProtocolError> {
    let var_type = proto::VarType::try_from(var_type).map_err(|_| ProtocolError::InvalidEnum {
        name: "VarType",
        value: var_type,
    })?;

    if var_type == VarType::Unspecified {
        return Err(ProtocolError::InvalidValue("AddVar.var_type"));
    }

    Ok(var_type)
}

fn from_var_direction_pb(var_direction: i32) -> Result<VarDirection, ProtocolError> {
    let var_direction =
        proto::VarDirection::try_from(var_direction).map_err(|_| ProtocolError::InvalidEnum {
            name: "VarDirection",
            value: var_direction,
        })?;

    if var_direction == VarDirection::Unspecified {
        return Err(ProtocolError::InvalidValue("AddVar.var_direction"));
    }

    Ok(var_direction)
}

fn to_signal_encoding_pb(signal_encoding: SignalEncoding) -> proto::SignalEncoding {
    let kind = match signal_encoding {
        SignalEncoding::String => proto::signal_encoding::Kind::String(proto::Empty {}),
        SignalEncoding::Real => proto::signal_encoding::Kind::Real(proto::Empty {}),
        SignalEncoding::BitVector(width) => proto::signal_encoding::Kind::BitVector(width),
    };

    proto::SignalEncoding { kind: Some(kind) }
}

fn from_signal_encoding_pb(
    signal_encoding: proto::SignalEncoding,
) -> Result<SignalEncoding, ProtocolError> {
    let kind = required_field(signal_encoding.kind, "SignalEncoding.kind")?;
    Ok(match kind {
        proto::signal_encoding::Kind::String(_) => SignalEncoding::String,
        proto::signal_encoding::Kind::Real(_) => SignalEncoding::Real,
        proto::signal_encoding::Kind::BitVector(width) => SignalEncoding::BitVector(width),
    })
}

fn to_time_step_pb(time_step: &TimeStep) -> proto::TimeStep {
    proto::TimeStep {
        timestamp: time_step.timestamp,
        changes: time_step.changes.iter().map(to_signal_change_pb).collect(),
    }
}

fn from_time_step_pb(time_step: proto::TimeStep) -> Result<TimeStep, ProtocolError> {
    Ok(TimeStep {
        timestamp: time_step.timestamp,
        changes: time_step
            .changes
            .into_iter()
            .map(from_signal_change_pb)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn to_signal_change_pb(signal_change: &SignalChange) -> proto::SignalChange {
    proto::SignalChange {
        id: signal_change.id,
        value: Some(to_signal_value_pb(&signal_change.value)),
    }
}

fn from_signal_change_pb(
    signal_change: proto::SignalChange,
) -> Result<SignalChange, ProtocolError> {
    Ok(SignalChange {
        id: signal_change.id,
        value: from_signal_value_pb(required_field(signal_change.value, "SignalChange.value")?)?,
    })
}

fn to_signal_value_pb(signal_value: &SignalValue) -> proto::SignalValue {
    let kind = match signal_value {
        SignalValue::VCDValue(value) => proto::signal_value::Kind::VcdValue(value.clone()),
        SignalValue::Real(value) => proto::signal_value::Kind::Real(*value),
        SignalValue::FSTValue(value) => proto::signal_value::Kind::FstValue(value.clone()),
    };

    proto::SignalValue { kind: Some(kind) }
}

fn from_signal_value_pb(signal_value: proto::SignalValue) -> Result<SignalValue, ProtocolError> {
    let kind = required_field(signal_value.kind, "SignalValue.kind")?;
    Ok(match kind {
        proto::signal_value::Kind::VcdValue(value) => SignalValue::VCDValue(value),
        proto::signal_value::Kind::Real(value) => SignalValue::Real(value),
        proto::signal_value::Kind::FstValue(value) => SignalValue::FSTValue(value),
    })
}

fn required_field<T>(field: Option<T>, name: &'static str) -> Result<T, ProtocolError> {
    field.ok_or(ProtocolError::MissingField(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_value_changes_encoding_matches_normal_encoding() {
        let time_steps = vec![
            TimeStep {
                timestamp: 4,
                changes: vec![SignalChange {
                    id: 7,
                    value: SignalValue::VCDValue(vec![b'b', b'1']),
                }],
            },
            TimeStep {
                timestamp: 128,
                changes: vec![SignalChange {
                    id: 9,
                    value: SignalValue::VCDValue(vec![b'0']),
                }],
            },
        ];
        let message = SimulatorToSurferMessage::ValueChanges {
            time_steps: time_steps.clone(),
            status: SimulationStatus::Done,
        };

        let mut buffer = Vec::new();
        for time_step in &time_steps {
            append_value_changes_time_step(&mut buffer, time_step);
        }
        let incremental = encode_value_changes_buffer(&buffer, SimulationStatus::Done);

        assert_eq!(
            incremental,
            encode_simulator_to_surfer_message(&message).unwrap()
        );

        let decoded = decode_simulator_to_surfer_message(&incremental).unwrap();
        match decoded {
            SimulatorToSurferMessage::ValueChanges {
                time_steps: decoded_steps,
                status,
            } => {
                assert_eq!(decoded_steps.len(), 2);
                assert_eq!(decoded_steps[0].timestamp, 4);
                assert_eq!(decoded_steps[1].timestamp, 128);
                assert_eq!(decoded_steps[0].changes[0].id, 7);
                assert_eq!(decoded_steps[1].changes[0].id, 9);
                assert_eq!(status, SimulationStatus::Done);
            }
            _ => panic!("incremental payload decoded as the wrong message"),
        }
    }

    #[test]
    fn hierarchy_add_var_parameter_flag_roundtrips() {
        let message = SimulatorToSurferMessage::Hierarchy {
            hierarchy: vec![HierarchyElement::AddVar {
                id: 7,
                name: "parameter".to_string(),
                index: None,
                type_name: "integer".to_string(),
                var_type: VarType::Integer,
                signal_encoding: SignalEncoding::BitVector(32),
                var_direction: None,
                enum_map: None,
                is_parameter: true,
            }],
            exponent: -9,
        };

        let encoded = encode_simulator_to_surfer_message(&message).unwrap();
        let decoded = decode_simulator_to_surfer_message(&encoded).unwrap();
        let SimulatorToSurferMessage::Hierarchy { hierarchy, .. } = decoded else {
            panic!("hierarchy payload decoded as the wrong message");
        };
        let Some(HierarchyElement::AddVar { is_parameter, .. }) = hierarchy.first() else {
            panic!("hierarchy payload did not contain an AddVar");
        };
        assert!(*is_parameter);
    }
}
