use serde::{Deserialize, Serialize};

pub use wellen::VarDirection;

#[derive(Serialize, Deserialize, Debug)]
pub enum PluginToSurferMessage {
    SimulatorName(String),
    Hierarchy(Vec<HierarchyElem>),
    ValueChanges(Vec<ValueChange>),
    Acknowledge(bool, SurferToPluginMessage), //for messages with no return, did it fail or not
    SendChangedValues(Vec<TimeStep>)
}

#[derive(Serialize, Deserialize, Debug)]
pub enum SurferToPluginMessage {
    GetSimulatorName,
    StartSimulation,
    PauseSimulation,
    StepSimulation,
    PollSimulation,
    GetHierarchy,
    SetCallback(u8), //TODO: Check if u8 is a good choice here
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
        kind: SignalKind,
        direction: VarDirection,
    },
}

// These types mirror what's available in rust_vhpi, but we need to implement some other traits, also this makes it possible to add conversions for eg. vpi in the future

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Signal {
    pub name: String,
    pub kind: SignalKind,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeStep{
    pub time_fs: u64,
    pub changes: Vec<SignalChange>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignalChange{
    pub signal_id: u32,
    pub value: u8, //TODO:Kommer behöva byta från u8 till möjligtvis Value från rust-vhpi repot vi fått (Value.rs filen)
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Time(pub i64);

// TODO: Add more fields and check that this cover what we can get from VHPI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    Scalar(ScalarValue),
    Vector(Vec<ScalarValue>),
    Real(f64),
    String(String),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScalarValue {
    U,
    X,
    Zero,
    One,
    Z,
    W,
    L,
    H,
    DontCare,
    Unknown(u8),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueChange {
    pub timestamp: Time,
    pub signal_name: String,
    pub value: Value,
}

// Conversions
impl From<vhpi::Time> for Time {
    fn from(t: vhpi::Time) -> Self {
        Self(t.to_i64())
    }
}

impl From<vhpi::Value> for Value {
    fn from(v: vhpi::Value) -> Self {
        match v {
            vhpi::Value::Logic(s) => Self::Scalar(s.into()),
            vhpi::Value::LogicVec(bits) => Self::Vector(bits.into_iter().map(Into::into).collect()),
            vhpi::Value::Real(f) => Self::Real(f),
            vhpi::Value::Str(s) => Self::String(s),
            vhpi::Value::Unknown => Self::Unknown,
            _ => Self::Unknown,
        }
    }
}

impl From<vhpi::LogicVal> for ScalarValue {
    fn from(l: vhpi::LogicVal) -> Self {
        match l {
            vhpi::LogicVal::U => ScalarValue::U,
            vhpi::LogicVal::X => ScalarValue::X,
            vhpi::LogicVal::Zero => ScalarValue::Zero,
            vhpi::LogicVal::One => ScalarValue::One,
            vhpi::LogicVal::Z => ScalarValue::Z,
            vhpi::LogicVal::W => ScalarValue::W,
            vhpi::LogicVal::L => ScalarValue::L,
            vhpi::LogicVal::H => ScalarValue::H,
            vhpi::LogicVal::DontCare => ScalarValue::DontCare,
            vhpi::LogicVal::Unknown(v) => ScalarValue::Unknown(v),
        }
    }
}
