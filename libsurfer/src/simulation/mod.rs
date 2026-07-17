use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub mod command_output;
#[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
pub mod simulation;
#[cfg(target_arch = "wasm32")]
use tokio_util as _; // Avoid unused import warning for tokio_util in wasm build, since it's only used in the simulation module which is not compiled for wasm.

#[cfg(not(target_arch = "wasm32"))]
pub mod simulator;

#[cfg(not(target_arch = "wasm32"))]
pub use simulator::{DesignEntity, DesignLibrary, DesignSelectionTree};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulatorInfo {
    pub protocol_version: u32,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Represents a design that can be simulated, either an entity or an architecture.
pub enum SimulatorDesign {
    Entity {
        entity: String,
    },
    Architecture {
        library: String,
        entity: String,
        architecture: String,
    },
    ExternalSimulator,
}

impl std::fmt::Display for SimulatorDesign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SimulatorDesign::Entity { entity } => write!(f, "{entity}"),
            SimulatorDesign::Architecture {
                library,
                entity,
                architecture,
            } => {
                write!(f, "{library}.{entity}.{architecture}")
            }
            SimulatorDesign::ExternalSimulator => write!(f, "External simulator"),
        }
    }
}
