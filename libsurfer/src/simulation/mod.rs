use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(not(target_arch = "wasm32"))]
pub mod command_output;
#[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
pub mod simulation_core;

#[cfg(not(target_arch = "wasm32"))]
pub mod simulator;
#[cfg(not(target_arch = "wasm32"))]
pub mod simulators;

#[cfg(not(target_arch = "wasm32"))]
pub use simulator::{DesignEntity, DesignLibrary, DesignSelectionTree};

use crate::{
    SystemState,
    displayed_item::{DisplayedItem, DisplayedItemRef},
    wave_container::WaveContainer,
};

use surfer_simulator_protocol::SimId;

impl SystemState {
    pub(crate) fn simulation_running(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .and_then(|waves| waves.inner.simulation_status())
            .is_some_and(|status| status == crate::wave_container::SimulationStatus::Running)
    }

    pub(crate) fn simulation_starting_or_shutting_down(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .and_then(|waves| waves.inner.simulation_status())
            .is_some_and(|status| {
                matches!(
                    status,
                    crate::wave_container::SimulationStatus::Starting
                        | crate::wave_container::SimulationStatus::ShuttingDown
                )
            })
    }
}

#[cfg(all(not(feature = "vscode"), target_arch = "wasm32"))]
impl SystemState {
    pub(crate) fn is_simulation(&self) -> bool {
        false
    }

    /// Placeholder function for the `simulator_track_variables` method when not running in VSCode or on wasm32.
    pub(crate) fn simulator_track_variables(
        &mut self,
        _vars: &[crate::wave_container::VariableRef],
    ) {
    }

    /// Placeholder function for the `simulator_untrack_variables` method when not running in VSCode or on wasm32.
    pub(crate) fn simulator_untrack_variables(&mut self, _ids: &[SimId]) {}

    /// Placeholder function for the `simulator_run_for_duration` method when not running in VSCode or on wasm32.
    pub(crate) fn simulator_run_for_duration(&mut self, duration: Option<u64>) {}
}

#[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
pub(crate) fn displayed_item_ref_to_sim_id_if_last_instance(
    item_ref: DisplayedItemRef,
    displayed_items: &HashMap<DisplayedItemRef, DisplayedItem>,
    container: Option<&WaveContainer>,
    signal_ref_to_id: Option<&AHashMap<wellen::SignalRef, SimId>>,
    is_simulation: bool,
) -> Option<SimId> {
    if !is_simulation {
        return None;
    }

    use crate::wave_container::SignalId;

    let container = container?;

    let found_id = if let Some(DisplayedItem::Variable(var)) = displayed_items.get(&item_ref)
        && let Ok(SignalId::Wellen(signal_ref)) = container.signal_id(&var.variable_ref)
    {
        signal_ref_to_id
            .and_then(|map| map.get(&signal_ref))
            .copied()
    } else {
        None
    }?;

    let is_last_instance = !displayed_items.iter().any(|(other_ref, item)| {
        if other_ref != &item_ref
            && let DisplayedItem::Variable(other_var) = item
            && let Ok(SignalId::Wellen(other_sig_ref)) =
                container.signal_id(&other_var.variable_ref)
        {
            return signal_ref_to_id
                .and_then(|map| map.get(&other_sig_ref))
                .copied()
                == Some(found_id);
        }
        false
    });

    is_last_instance.then_some(found_id)
}

#[cfg(not(any(feature = "vscode", not(target_arch = "wasm32"))))]
pub(crate) fn displayed_item_ref_to_sim_id_if_last_instance(
    item_ref: DisplayedItemRef,
    displayed_items: &HashMap<DisplayedItemRef, DisplayedItem>,
    container: Option<&WaveContainer>,
    signal_ref_to_id: Option<&AHashMap<wellen::SignalRef, SimId>>,
    is_simulation: bool,
) -> Option<SimId> {
    let _ = (
        item_ref,
        displayed_items,
        container,
        signal_ref_to_id,
        is_simulation,
    );
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulatorInfo {
    pub protocol_version: u32,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Represents a design that can be simulated.
pub enum SimulatorDesign {
    Module {
        module: String,
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
            SimulatorDesign::Module { module } => write!(f, "{module}"),
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
