#![deny(unused_crate_dependencies)]

use std::error::Error;
use std::fmt::{Display, Formatter};

use prost::Message;
use serde::{Deserialize, Serialize};
use surfer_time_types::{TimeScale, TimeUnit};
use wellen::{ScopeType, SignalEncoding, VarDirection, VarIndex, VarType};

/// Identifier assigned to a variable by the simulator.
pub type SimId = u32;

pub const DEFAULT_SIMULATOR_PORT: u16 = 6967;

mod proto {
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
    let pb = to_surfer_to_simulator_pb(message);
    Ok(pb.encode_to_vec())
}

pub fn decode_surfer_to_simulator_message(
    data: &[u8],
) -> Result<SurferToSimulatorMessage, ProtocolError> {
    let pb = proto::SurferToSimulator::decode(data)?;
    from_surfer_to_simulator_pb(pb)
}

pub fn encode_simulator_to_surfer_message(
    message: &SimulatorToSurferMessage,
) -> Result<Vec<u8>, ProtocolError> {
    let pb = to_simulator_to_surfer_pb(message);
    Ok(pb.encode_to_vec())
}

pub fn decode_simulator_to_surfer_message(
    data: &[u8],
) -> Result<SimulatorToSurferMessage, ProtocolError> {
    let pb = proto::SimulatorToSurfer::decode(data)?;
    from_simulator_to_surfer_pb(pb)
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
        time_scale: TimeScale,
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
        /// Scope name.
        scope: Option<String>,
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
    /// Encoded value as it would appear in a VCD file, without the timestamp or variable id.
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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum SimulationStatus {
    /// Simulation is finished and no more value changes will be sent.
    Done,
    /// Simulation is paused and no more value changes will be sent until resumed.
    Paused,
    /// Simulation is still running and more value changes will be sent.
    Running,
}

fn to_surfer_to_simulator_pb(message: &SurferToSimulatorMessage) -> proto::SurferToSimulator {
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

    proto::SurferToSimulator { kind: Some(kind) }
}

fn from_surfer_to_simulator_pb(
    message: proto::SurferToSimulator,
) -> Result<SurferToSimulatorMessage, ProtocolError> {
    let kind = required_field(message.kind, "SurferToSimulator.kind")?;
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

fn to_simulator_to_surfer_pb(message: &SimulatorToSurferMessage) -> proto::SimulatorToSurfer {
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
            time_scale,
        } => proto::simulator_to_surfer::Kind::Hierarchy(proto::Hierarchy {
            hierarchy: hierarchy.iter().map(to_hierarchy_element_pb).collect(),
            time_scale: Some(to_time_scale_pb(time_scale)),
        }),
        SimulatorToSurferMessage::Acknowledge { success } => {
            proto::simulator_to_surfer::Kind::Acknowledge(proto::Acknowledge { success: *success })
        }
        SimulatorToSurferMessage::ValueChanges { time_steps, status } => {
            proto::simulator_to_surfer::Kind::ValueChanges(proto::ValueChanges {
                time_steps: time_steps.iter().map(to_time_step_pb).collect(),
                status: to_simulation_status_pb(*status) as i32,
            })
        }
        SimulatorToSurferMessage::CurrentSimulationTime { time } => {
            proto::simulator_to_surfer::Kind::CurrentSimulationTime(proto::CurrentSimulationTime {
                time: *time,
            })
        }
    };

    proto::SimulatorToSurfer { kind: Some(kind) }
}

fn from_simulator_to_surfer_pb(
    message: proto::SimulatorToSurfer,
) -> Result<SimulatorToSurferMessage, ProtocolError> {
    let kind = required_field(message.kind, "SimulatorToSurfer.kind")?;
    match kind {
        proto::simulator_to_surfer::Kind::SimulatorInfo(info) => {
            Ok(SimulatorToSurferMessage::SimulatorInfo {
                protocol_version: info.protocol_version,
                name: info.name,
                version: info.version,
            })
        }
        proto::simulator_to_surfer::Kind::Hierarchy(hierarchy) => {
            let time_scale = required_field(hierarchy.time_scale, "Hierarchy.time_scale")?;
            Ok(SimulatorToSurferMessage::Hierarchy {
                hierarchy: hierarchy
                    .hierarchy
                    .into_iter()
                    .map(from_hierarchy_element_pb)
                    .collect::<Result<Vec<_>, _>>()?,
                time_scale: from_time_scale_pb(time_scale)?,
            })
        }
        proto::simulator_to_surfer::Kind::Acknowledge(ack) => {
            Ok(SimulatorToSurferMessage::Acknowledge {
                success: ack.success,
            })
        }
        proto::simulator_to_surfer::Kind::ValueChanges(changes) => {
            let status = from_simulation_status_pb(changes.status)?;
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

fn to_time_scale_pb(time_scale: &TimeScale) -> proto::TimeScale {
    proto::TimeScale {
        unit: to_time_unit_pb(time_scale.unit) as i32,
        multiplier: time_scale.multiplier,
    }
}

fn from_time_scale_pb(time_scale: proto::TimeScale) -> Result<TimeScale, ProtocolError> {
    let unit = from_time_unit_pb(time_scale.unit)?;
    Ok(TimeScale {
        unit,
        multiplier: time_scale.multiplier,
    })
}

fn to_time_unit_pb(unit: TimeUnit) -> proto::TimeUnit {
    match unit {
        TimeUnit::ZeptoSeconds => proto::TimeUnit::ZeptoSeconds,
        TimeUnit::AttoSeconds => proto::TimeUnit::AttoSeconds,
        TimeUnit::FemtoSeconds => proto::TimeUnit::FemtoSeconds,
        TimeUnit::PicoSeconds => proto::TimeUnit::PicoSeconds,
        TimeUnit::NanoSeconds => proto::TimeUnit::NanoSeconds,
        TimeUnit::MicroSeconds => proto::TimeUnit::MicroSeconds,
        TimeUnit::MilliSeconds => proto::TimeUnit::MilliSeconds,
        TimeUnit::Seconds => proto::TimeUnit::Seconds,
        TimeUnit::None => proto::TimeUnit::None,
        TimeUnit::Auto => proto::TimeUnit::Auto,
    }
}

fn from_time_unit_pb(unit: i32) -> Result<TimeUnit, ProtocolError> {
    let unit = proto::TimeUnit::try_from(unit).map_err(|_| ProtocolError::InvalidEnum {
        name: "TimeUnit",
        value: unit,
    })?;

    Ok(match unit {
        proto::TimeUnit::Unspecified => TimeUnit::None,
        proto::TimeUnit::ZeptoSeconds => TimeUnit::ZeptoSeconds,
        proto::TimeUnit::AttoSeconds => TimeUnit::AttoSeconds,
        proto::TimeUnit::FemtoSeconds => TimeUnit::FemtoSeconds,
        proto::TimeUnit::PicoSeconds => TimeUnit::PicoSeconds,
        proto::TimeUnit::NanoSeconds => TimeUnit::NanoSeconds,
        proto::TimeUnit::MicroSeconds => TimeUnit::MicroSeconds,
        proto::TimeUnit::MilliSeconds => TimeUnit::MilliSeconds,
        proto::TimeUnit::Seconds => TimeUnit::Seconds,
        proto::TimeUnit::None => TimeUnit::None,
        proto::TimeUnit::Auto => TimeUnit::Auto,
    })
}

fn to_hierarchy_element_pb(element: &HierarchyElement) -> proto::HierarchyElement {
    let kind = match element {
        HierarchyElement::PushScope { name, scope_type } => {
            proto::hierarchy_element::Kind::PushScope(proto::PushScope {
                name: name.clone(),
                scope_type: to_scope_type_pb(*scope_type) as i32,
            })
        }
        HierarchyElement::PopScope => proto::hierarchy_element::Kind::PopScope(proto::Empty {}),
        HierarchyElement::AddVar {
            id,
            name,
            index,
            scope,
            type_name,
            var_type,
            signal_encoding,
            var_direction,
            enum_map,
        } => proto::hierarchy_element::Kind::AddVar(proto::AddVar {
            id: *id,
            name: name.clone(),
            index: index.as_ref().map(to_var_index_pb),
            scope: scope.clone(),
            type_name: type_name.clone(),
            var_type: to_var_type_pb(*var_type) as i32,
            signal_encoding: Some(to_signal_encoding_pb(*signal_encoding)),
            var_direction: var_direction
                .as_ref()
                .copied()
                .map(|direction| to_var_direction_pb(direction) as i32),
            enum_map: enum_map.as_ref().map(|entries| proto::EnumMap {
                entries: entries
                    .iter()
                    .map(|(key, value)| proto::EnumMapEntry {
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .collect(),
            }),
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
                index: add_var.index.map(from_var_index_pb),
                scope: add_var.scope,
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
            })
        }
    }
}

fn to_scope_type_pb(scope_type: ScopeType) -> proto::ScopeType {
    match scope_type {
        ScopeType::Module => proto::ScopeType::Module,
        ScopeType::Task => proto::ScopeType::Task,
        ScopeType::Function => proto::ScopeType::Function,
        ScopeType::Begin => proto::ScopeType::Begin,
        ScopeType::Fork => proto::ScopeType::Fork,
        ScopeType::Generate => proto::ScopeType::Generate,
        ScopeType::Struct => proto::ScopeType::Struct,
        ScopeType::Union => proto::ScopeType::Union,
        ScopeType::Class => proto::ScopeType::Class,
        ScopeType::Interface => proto::ScopeType::Interface,
        ScopeType::Package => proto::ScopeType::Package,
        ScopeType::Program => proto::ScopeType::Program,
        ScopeType::VhdlArchitecture => proto::ScopeType::VhdlArchitecture,
        ScopeType::VhdlProcedure => proto::ScopeType::VhdlProcedure,
        ScopeType::VhdlFunction => proto::ScopeType::VhdlFunction,
        ScopeType::VhdlRecord => proto::ScopeType::VhdlRecord,
        ScopeType::VhdlProcess => proto::ScopeType::VhdlProcess,
        ScopeType::VhdlBlock => proto::ScopeType::VhdlBlock,
        ScopeType::VhdlForGenerate => proto::ScopeType::VhdlForGenerate,
        ScopeType::VhdlIfGenerate => proto::ScopeType::VhdlIfGenerate,
        ScopeType::VhdlGenerate => proto::ScopeType::VhdlGenerate,
        ScopeType::VhdlPackage => proto::ScopeType::VhdlPackage,
        ScopeType::GhwGeneric => proto::ScopeType::GhwGeneric,
        ScopeType::VhdlArray => proto::ScopeType::VhdlArray,
        ScopeType::Unknown => proto::ScopeType::Unknown,
        ScopeType::Clocking => proto::ScopeType::Clocking,
        ScopeType::SvArray => proto::ScopeType::SvArray,
        _ => proto::ScopeType::Unknown,
    }
}

fn from_scope_type_pb(scope_type: i32) -> Result<ScopeType, ProtocolError> {
    let scope_type =
        proto::ScopeType::try_from(scope_type).map_err(|_| ProtocolError::InvalidEnum {
            name: "ScopeType",
            value: scope_type,
        })?;

    Ok(match scope_type {
        proto::ScopeType::Unspecified => ScopeType::Unknown,
        proto::ScopeType::Module => ScopeType::Module,
        proto::ScopeType::Task => ScopeType::Task,
        proto::ScopeType::Function => ScopeType::Function,
        proto::ScopeType::Begin => ScopeType::Begin,
        proto::ScopeType::Fork => ScopeType::Fork,
        proto::ScopeType::Generate => ScopeType::Generate,
        proto::ScopeType::Struct => ScopeType::Struct,
        proto::ScopeType::Union => ScopeType::Union,
        proto::ScopeType::Class => ScopeType::Class,
        proto::ScopeType::Interface => ScopeType::Interface,
        proto::ScopeType::Package => ScopeType::Package,
        proto::ScopeType::Program => ScopeType::Program,
        proto::ScopeType::VhdlArchitecture => ScopeType::VhdlArchitecture,
        proto::ScopeType::VhdlProcedure => ScopeType::VhdlProcedure,
        proto::ScopeType::VhdlFunction => ScopeType::VhdlFunction,
        proto::ScopeType::VhdlRecord => ScopeType::VhdlRecord,
        proto::ScopeType::VhdlProcess => ScopeType::VhdlProcess,
        proto::ScopeType::VhdlBlock => ScopeType::VhdlBlock,
        proto::ScopeType::VhdlForGenerate => ScopeType::VhdlForGenerate,
        proto::ScopeType::VhdlIfGenerate => ScopeType::VhdlIfGenerate,
        proto::ScopeType::VhdlGenerate => ScopeType::VhdlGenerate,
        proto::ScopeType::VhdlPackage => ScopeType::VhdlPackage,
        proto::ScopeType::GhwGeneric => ScopeType::GhwGeneric,
        proto::ScopeType::VhdlArray => ScopeType::VhdlArray,
        proto::ScopeType::Unknown => ScopeType::Unknown,
        proto::ScopeType::Clocking => ScopeType::Clocking,
        proto::ScopeType::SvArray => ScopeType::SvArray,
    })
}

fn to_var_type_pb(var_type: VarType) -> proto::VarType {
    match var_type {
        VarType::Event => proto::VarType::Event,
        VarType::Integer => proto::VarType::Integer,
        VarType::Parameter => proto::VarType::Parameter,
        VarType::Real => proto::VarType::Real,
        VarType::Reg => proto::VarType::Reg,
        VarType::Supply0 => proto::VarType::Supply0,
        VarType::Supply1 => proto::VarType::Supply1,
        VarType::Time => proto::VarType::Time,
        VarType::Tri => proto::VarType::Tri,
        VarType::TriAnd => proto::VarType::TriAnd,
        VarType::TriOr => proto::VarType::TriOr,
        VarType::TriReg => proto::VarType::TriReg,
        VarType::Tri0 => proto::VarType::Tri0,
        VarType::Tri1 => proto::VarType::Tri1,
        VarType::WAnd => proto::VarType::WAnd,
        VarType::Wire => proto::VarType::Wire,
        VarType::WOr => proto::VarType::WOr,
        VarType::String => proto::VarType::String,
        VarType::Port => proto::VarType::Port,
        VarType::SparseArray => proto::VarType::SparseArray,
        VarType::RealTime => proto::VarType::RealTime,
        VarType::RealParameter => proto::VarType::RealParameter,
        VarType::Bit => proto::VarType::Bit,
        VarType::Logic => proto::VarType::Logic,
        VarType::Int => proto::VarType::Int,
        VarType::ShortInt => proto::VarType::ShortInt,
        VarType::LongInt => proto::VarType::LongInt,
        VarType::Byte => proto::VarType::Byte,
        VarType::Enum => proto::VarType::Enum,
        VarType::ShortReal => proto::VarType::ShortReal,
        VarType::Boolean => proto::VarType::Boolean,
        VarType::BitVector => proto::VarType::BitVector,
        VarType::StdLogic => proto::VarType::StdLogic,
        VarType::StdLogicVector => proto::VarType::StdLogicVector,
        VarType::StdULogic => proto::VarType::StdULogic,
        VarType::StdULogicVector => proto::VarType::StdULogicVector,
        VarType::EventParameter => proto::VarType::EventParameter,
    }
}

fn from_var_type_pb(var_type: i32) -> Result<VarType, ProtocolError> {
    let var_type = proto::VarType::try_from(var_type).map_err(|_| ProtocolError::InvalidEnum {
        name: "VarType",
        value: var_type,
    })?;

    match var_type {
        proto::VarType::Unspecified => Err(ProtocolError::InvalidValue("AddVar.var_type")),
        proto::VarType::Event => Ok(VarType::Event),
        proto::VarType::Integer => Ok(VarType::Integer),
        proto::VarType::Parameter => Ok(VarType::Parameter),
        proto::VarType::Real => Ok(VarType::Real),
        proto::VarType::Reg => Ok(VarType::Reg),
        proto::VarType::Supply0 => Ok(VarType::Supply0),
        proto::VarType::Supply1 => Ok(VarType::Supply1),
        proto::VarType::Time => Ok(VarType::Time),
        proto::VarType::Tri => Ok(VarType::Tri),
        proto::VarType::TriAnd => Ok(VarType::TriAnd),
        proto::VarType::TriOr => Ok(VarType::TriOr),
        proto::VarType::TriReg => Ok(VarType::TriReg),
        proto::VarType::Tri0 => Ok(VarType::Tri0),
        proto::VarType::Tri1 => Ok(VarType::Tri1),
        proto::VarType::WAnd => Ok(VarType::WAnd),
        proto::VarType::Wire => Ok(VarType::Wire),
        proto::VarType::WOr => Ok(VarType::WOr),
        proto::VarType::String => Ok(VarType::String),
        proto::VarType::Port => Ok(VarType::Port),
        proto::VarType::SparseArray => Ok(VarType::SparseArray),
        proto::VarType::RealTime => Ok(VarType::RealTime),
        proto::VarType::RealParameter => Ok(VarType::RealParameter),
        proto::VarType::Bit => Ok(VarType::Bit),
        proto::VarType::Logic => Ok(VarType::Logic),
        proto::VarType::Int => Ok(VarType::Int),
        proto::VarType::ShortInt => Ok(VarType::ShortInt),
        proto::VarType::LongInt => Ok(VarType::LongInt),
        proto::VarType::Byte => Ok(VarType::Byte),
        proto::VarType::Enum => Ok(VarType::Enum),
        proto::VarType::ShortReal => Ok(VarType::ShortReal),
        proto::VarType::Boolean => Ok(VarType::Boolean),
        proto::VarType::BitVector => Ok(VarType::BitVector),
        proto::VarType::StdLogic => Ok(VarType::StdLogic),
        proto::VarType::StdLogicVector => Ok(VarType::StdLogicVector),
        proto::VarType::StdULogic => Ok(VarType::StdULogic),
        proto::VarType::StdULogicVector => Ok(VarType::StdULogicVector),
        proto::VarType::EventParameter => Ok(VarType::EventParameter),
    }
}

fn to_var_direction_pb(var_direction: VarDirection) -> proto::VarDirection {
    match var_direction {
        VarDirection::Unknown => proto::VarDirection::Unknown,
        VarDirection::Implicit => proto::VarDirection::Implicit,
        VarDirection::Input => proto::VarDirection::Input,
        VarDirection::Output => proto::VarDirection::Output,
        VarDirection::InOut => proto::VarDirection::InOut,
        VarDirection::Buffer => proto::VarDirection::Buffer,
        VarDirection::Linkage => proto::VarDirection::Linkage,
    }
}

fn from_var_direction_pb(var_direction: i32) -> Result<VarDirection, ProtocolError> {
    let var_direction =
        proto::VarDirection::try_from(var_direction).map_err(|_| ProtocolError::InvalidEnum {
            name: "VarDirection",
            value: var_direction,
        })?;

    Ok(match var_direction {
        proto::VarDirection::Unspecified => VarDirection::Unknown,
        proto::VarDirection::Unknown => VarDirection::Unknown,
        proto::VarDirection::Implicit => VarDirection::Implicit,
        proto::VarDirection::Input => VarDirection::Input,
        proto::VarDirection::Output => VarDirection::Output,
        proto::VarDirection::InOut => VarDirection::InOut,
        proto::VarDirection::Buffer => VarDirection::Buffer,
        proto::VarDirection::Linkage => VarDirection::Linkage,
    })
}

fn to_var_index_pb(var_index: &VarIndex) -> proto::VarIndex {
    proto::VarIndex {
        msb: var_index.msb(),
        lsb: var_index.lsb(),
    }
}

fn from_var_index_pb(var_index: proto::VarIndex) -> VarIndex {
    VarIndex::new(var_index.msb, var_index.lsb)
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

fn to_simulation_status_pb(value: SimulationStatus) -> proto::SimulationStatus {
    match value {
        SimulationStatus::Done => proto::SimulationStatus::Done,
        SimulationStatus::Paused => proto::SimulationStatus::Paused,
        SimulationStatus::Running => proto::SimulationStatus::Running,
    }
}

fn from_simulation_status_pb(value: i32) -> Result<SimulationStatus, ProtocolError> {
    let value =
        proto::SimulationStatus::try_from(value).map_err(|_| ProtocolError::InvalidEnum {
            name: "SimulationStatus",
            value,
        })?;

    match value {
        proto::SimulationStatus::Unspecified => {
            Err(ProtocolError::InvalidValue("ValueChanges.status"))
        }
        proto::SimulationStatus::Done => Ok(SimulationStatus::Done),
        proto::SimulationStatus::Paused => Ok(SimulationStatus::Paused),
        proto::SimulationStatus::Running => Ok(SimulationStatus::Running),
    }
}

fn required_field<T>(field: Option<T>, name: &'static str) -> Result<T, ProtocolError> {
    field.ok_or(ProtocolError::MissingField(name))
}
