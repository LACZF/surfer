use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub mod command_output;
#[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
pub mod simulation_core;

use crate::{
    SystemState,
    displayed_item::{DisplayedItem, DisplayedItemRef},
    wave_container::{VarId, VariableRef, WaveContainer},
    wave_source::WaveSource,
};

impl SystemState {
    #[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
    pub(crate) fn is_simulation(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .map(|waves| matches!(waves.source, WaveSource::SimulatorPlugin(_)))
            .unwrap_or(false)
    }

    #[cfg(not(any(feature = "vscode", not(target_arch = "wasm32"))))]
    #[inline]
    pub(crate) fn is_simulation(&self) -> bool {
        false
    }

    pub(crate) fn simulation_running(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .and_then(|waves| waves.inner.simulation_status())
            .is_some_and(|status| status == crate::wave_container::SimulationStatus::Running)
    }

    pub(crate) fn wellen_var_ids(&self, vars: &[VariableRef]) -> Vec<u32> {
        if !self.is_simulation() {
            return vec![];
        }

        vars.iter()
            .filter_map(|var| match &var.id {
                VarId::Wellen(var_ref) => Some(var_ref.index() as u32),
                _ => {
                    tracing::error!("This was not a Wellen-variable");
                    None
                }
            })
            .collect()
    }
}

pub(crate) fn wellen_signal_ids_for_tracking_from_state(
    vars: &[VariableRef],
    container: Option<&WaveContainer>,
    signal_ref_to_id: Option<&HashMap<wellen::SignalRef, u32>>,
    is_simulation: bool,
) -> Vec<u32> {
    if !is_simulation {
        return vec![];
    }

    #[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
    {
        use crate::wave_container::SignalId;
        container
            .map(|container| {
                vars.iter()
                    .filter_map(|var| {
                        if let Ok(SignalId::Wellen(signal_ref)) = container.signal_id(var)
                            && let Some(id) = signal_ref_to_id
                                .and_then(|map| map.get(&signal_ref))
                                .copied()
                        {
                            Some(id)
                        } else {
                            tracing::info!("Couldn't finds a Wellen-ID for: {}", var.name);
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(not(any(feature = "vscode", not(target_arch = "wasm32"))))]
    {
        let _ = (vars, container, signal_ref_to_id);
        vec![]
    }
}

pub(crate) fn wellen_signal_id_to_untrack_for_removed_item_from_state(
    item_ref: DisplayedItemRef,
    displayed_items: &HashMap<DisplayedItemRef, DisplayedItem>,
    container: Option<&WaveContainer>,
    signal_ref_to_id: Option<&HashMap<wellen::SignalRef, u32>>,
    is_simulation: bool,
) -> Option<u32> {
    if !is_simulation {
        return None;
    }

    #[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
    {
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
    {
        let _ = (item_ref, displayed_items, container, signal_ref_to_id);
        None
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub mod simulator;
#[cfg(not(target_arch = "wasm32"))]
pub mod simulators;

#[cfg(not(target_arch = "wasm32"))]
pub use simulator::{DesignEntity, DesignLibrary, DesignSelectionTree};

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
