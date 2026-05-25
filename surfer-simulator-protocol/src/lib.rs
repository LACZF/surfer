#![deny(unused_crate_dependencies)]

use serde::{Deserialize, Serialize};

use surfer_time_types::TimeScale;

use wellen::{ScopeType, SignalEncoding, VarDirection, VarIndex, VarType};

/// Messages sent by the simulator to Surfer
#[derive(Serialize, Deserialize, Debug)]
pub enum SimulatorToSurferMessage {
    /// Basic simulator metadata.
    SimulatorInfo { name: String, version: String },
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
    ///
    /// The boolean is `true` when there are no more value changes to send.
    ValueChanges {
        time_steps: Vec<TimeStep>,
        status: ValueChangeStatus,
    },
    /// Current simulation time in simulation time units.
    /// This is used to update the user interface.
    /// It is recommended that it is sent based on wall time (e.g. every 500ms)
    /// rather than every time the simulation time changes, to avoid overwhelming the UI with updates.
    CurrentSimulationTime { time: i64 },
}

/// Messages sent by Surfer to the simulator
#[derive(Serialize, Deserialize, Debug)]
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
    /// Start reporting value changes for the variable identified by hierarchy id.
    TrackVariableChanges { id: u32 },
    /// Stop reporting value changes for the variable identified by hierarchy id.
    UntrackVariableChanges { id: u32 },
    /// Request currently buffered value changes.
    GetValueChanges,
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
        id: u32,
        /// Variable name.
        name: String,
        /// Index for arrays or multi-dimensional variables, or `None` for scalars.
        index: Option<VarIndex>,
        /// Scope name
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
    pub timestamp: i64,
    /// Signal changes observed at this timestamp.
    pub changes: Vec<SignalChange>,
}

/// A change for a single value
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignalChange {
    /// Variable id from hierarchy.
    pub id: u32,
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ValueChangeStatus {
    /// Simulation is finished and no more value changes will be sent.
    Done,
    /// Simulation is paused and no more value changes will be sent until resumed.
    Paused,
    /// Simulation is still running and more value changes will be sent.
    Running,
}
