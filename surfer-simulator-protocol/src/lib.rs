use serde::{Deserialize, Serialize};

pub use wellen::VarDirection;

/// Messages sent by the plugin to Surfer
#[derive(Serialize, Deserialize, Debug)]
pub enum PluginToSurferMessage {
    SimulatorName(String),
    Hierarchy(Vec<HierarchyElem>, TimeUnit),
    Acknowledge(bool, SurferToPluginMessage), //for messages with no return, did it fail or not
    SendChangedValues(Vec<TimeStep>),
}

/// Messages sent by Surfer to the Plugin
#[derive(Serialize, Deserialize, Debug)]
pub enum SurferToPluginMessage {
    GetSimulatorName,
    StartSimulation,
    PauseSimulation,
    StepSimulation,
    PollSimulation,
    GetHierarchy,
    SetCallback(SignalID),
    SendValues,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum HierarchyElem {
    PushScope {
        name: String,
    },
    PopScope,
    AddVar {
        name: String,
        id: SignalID,
        kind: SignalKind,
        direction: VarDirection, // TODO: Should we keep this? It's not available in VCD-files and I think we just make up some value right now
    },
}

/// Signal kind in hierarchy, with name to display in Surfer
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SignalKind {
    Array {
        type_name: String,
        elements: Vec<String>, // element names by index
    },
    Record {
        type_name: String,
        fields: Vec<String>,
    },
    Enum {
        type_name: String,
        literals: Vec<String>,
    },
    Scalar {
        type_name: String,
    },
    Unknown,
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
    pub id: SignalID,
    pub value: Vec<u8>, // Encoded in the same format as in a VCD-file
}

/// A unique ID for each signal, used in wellen and to refer to signals
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SignalID(pub u64);

impl From<u64> for SignalID {
    fn from(value: u64) -> Self {
        SignalID(value)
    }
}

impl From<SignalID> for u64 {
    fn from(id: SignalID) -> Self {
        id.0
    }
}
