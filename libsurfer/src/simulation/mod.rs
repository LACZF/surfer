use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub mod simulation;
#[cfg(target_arch = "wasm32")]
use tokio_util as _; // Avoid unused import warning for tokio_util in wasm build, since it's only used in the simulation module which is not compiled for wasm.

pub mod simulator;

pub(crate) use simulator::DesignSelectionTree;

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
