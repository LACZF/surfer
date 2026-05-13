use serde::{Deserialize, Serialize};

pub use wellen::{ScopeType, SignalEncoding, VarDirection, VarIndex, VarType};


/// Messages sent by the plugin to Surfer
#[derive(Serialize, Deserialize, Debug)]
pub enum PluginToSurferMessage {
    SimulatorName(String),
    Hierarchy(Vec<HierarchyElem>, TimeUnit),
    Acknowledge(bool), // For messages with no return, did it succeed or not?
    SendChangedValues(Vec<TimeStep>),
}

/// Messages sent by Surfer to the Plugin
#[derive(Serialize, Deserialize, Debug)]
pub enum SurferToPluginMessage {
    GetSimulatorName,
    RunSimulation(Option<u64>), // How much farther to simulate (or whole simulation)
    PauseSimulation,
    StepSimulation, // TODO: Remove?
    PollSimulation, // TODO: Remove?
    GetHierarchy,
    SetCallback(u64),    // id from hierarchy
    RemoveCallback(u64), // id from hierarchy
    SendValues,          // TODO: Remove?
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum HierarchyElem {
    PushScope {
        name: String,
        scope_type: wellen::ScopeType,
    },
    PopScope,
    AddVar {
        id: u64,
        name: String,
        type_name: String,
        length: u32,
        var_type: wellen::VarType,
        signal_encoding: wellen::SignalEncoding,
        // TODO: Add enum types
        // TODO: Add direction
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ScopeKind {
    Module,
    Array,
}

/// Signal kind in hierarchy, with name to display in Surfer
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SignalKind {
    Array {
        type_name: String,
        length: u32, // How many bits is the value
        var_type: wellen::VarType,
        signal_encoding: wellen::SignalEncoding,
        // TODO: Add enum types
        // TODO: Add direction
    },
}

/// What 1 unit of time represents when sending value changes
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum TimeUnit {
    ZeptoSeconds,
    AttoSeconds,
    FemtoSeconds,
    PicoSeconds,
    NanoSeconds,
    MicroSeconds,
    MilliSeconds,
    Seconds,
}

/// All recorded value changes at a certain timestep
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeStep {
    pub timestamp: i64,
    pub changes: Vec<SignalChange>,
}

/// A change for a single value
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignalChange {
    pub id: u64,
    pub value: Vec<u8>, // Encoded in the same format as in a VCD-file
}
