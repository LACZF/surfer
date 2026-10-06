use std::{
    collections::HashSet,
    process::{Child, ExitStatus},
};

#[cfg(not(test))]
use std::fs;

use crate::file_dialog::FileFilter;
use crate::simulation::{
    SimulatorDesign,
    simulators::{IcarusSimulator, NvcSimulator, XezimSimulator},
};
use camino::{Utf8Path, Utf8PathBuf};
use object::{Object, ObjectSection, ObjectSymbol};
use tracing::{info, warn};

#[cfg(test)]
const PLUGIN_ENV_VAR: &str = "SURFER_PLUGIN_TEST_PATH";

#[cfg(not(test))]
const PLUGIN_ENV_VAR: &str = "SURFER_PLUGIN_PATH";

include!(concat!(env!("OUT_DIR"), "/surfer_plugin_fingerprint.rs"));

#[derive(Debug, Clone, PartialEq, Eq)]
/// Represents a library of designs in the simulator's working directory.
pub struct DesignLibrary {
    pub name: String,
    pub entities: Vec<DesignEntity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Represents the available designs in the simulator's working directory, organized hierarchically.
pub enum DesignSelectionTree {
    Libraries(Vec<DesignLibrary>),
    Modules(Vec<String>),
    #[default]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Represents a design entity in the simulator's working directory.
pub struct DesignEntity {
    pub name: String,
    pub architectures: Vec<String>,
}

pub enum Sim {
    TwoStage(Box<dyn TwoStageSimulator>),
    OneStage(Box<dyn OneStageSimulator>),
}

impl Sim {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Sim::TwoStage(sim) => sim.name(),
            Sim::OneStage(sim) => sim.name(),
        }
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        match self {
            Sim::TwoStage(sim) => sim.is_available(),
            Sim::OneStage(sim) => sim.is_available(),
        }
    }

    #[must_use]
    pub fn compile_filter(&self) -> &'static FileFilter {
        match self {
            Sim::TwoStage(sim) => sim.compile_filter(),
            Sim::OneStage(sim) => sim.compile_filter(),
        }
    }

    #[must_use]
    pub fn get_designs(&self) -> DesignSelectionTree {
        match self {
            Sim::TwoStage(sim) => sim.get_designs(),
            Sim::OneStage(_) => DesignSelectionTree::Empty,
        }
    }

    pub fn set_workdir(&mut self, workdir: Option<&Utf8PathBuf>) {
        match self {
            Sim::TwoStage(sim) => sim.set_workdir(workdir),
            Sim::OneStage(sim) => sim.set_workdir(workdir),
        }
    }

    #[must_use]
    pub fn workdir(&self) -> Utf8PathBuf {
        match self {
            Sim::TwoStage(sim) => sim.workdir(),
            Sim::OneStage(sim) => sim.workdir(),
        }
    }

    #[must_use]
    pub fn plugin_stem(&self) -> &str {
        match self {
            Sim::TwoStage(sim) => sim.plugin_stem(),
            Sim::OneStage(sim) => sim.plugin_stem(),
        }
    }
}
/// Manages available simulators and their instantiation.
pub struct SimulatorList {
    simulators: Vec<Sim>,
}

impl SimulatorList {
    /// Create a new `SimulatorList` with all supported simulators.
    #[must_use]
    pub fn new(workdir: Option<&Utf8PathBuf>) -> Self {
        Self {
            simulators: vec![
                Sim::TwoStage(Box::new(IcarusSimulator::new(workdir))),
                Sim::TwoStage(Box::new(NvcSimulator::new(workdir))),
                Sim::OneStage(Box::new(XezimSimulator::new(workdir))),
            ],
        }
    }

    /// Get a list of available simulators, sorted alphabetically by name.
    #[must_use]
    pub fn get_available_simulators(&self) -> Vec<&Sim> {
        let mut available = self
            .simulators
            .iter()
            .filter(|simulator| simulator.is_available())
            .collect::<Vec<&Sim>>();

        available.sort_by_key(|simulator| simulator.name().to_ascii_lowercase());
        available
    }

    /// Get a simulator by name, if available.
    #[must_use]
    pub fn get_simulator_by_name(&self, name: &str) -> Option<&Sim> {
        self.get_available_simulators()
            .into_iter()
            .find(|simulator| simulator.name() == name)
    }

    /// Create a new instance of a simulator by name, if available.
    #[must_use]
    pub fn create_simulator_by_name(
        &self,
        name: &str,
        workdir: Option<&Utf8PathBuf>,
    ) -> Option<Sim> {
        match name {
            "NVC" => Some(Sim::TwoStage(Box::new(NvcSimulator::new(workdir)))),
            "Icarus Verilog" => Some(Sim::TwoStage(Box::new(IcarusSimulator::new(workdir)))),
            "Xezim" => Some(Sim::OneStage(Box::new(XezimSimulator::new(workdir)))),
            _ => None,
        }
    }

    /// Collect all designs from all available simulators.
    ///
    /// Returns tuples of `(design, simulator_name)`.
    pub(crate) fn all_designs(&self) -> Vec<(SimulatorDesign, String)> {
        let mut designs = Vec::new();

        for simulator in self.get_available_simulators() {
            let Sim::TwoStage(simulator) = simulator else {
                continue;
            };
            let simulator_name = simulator.name().to_string();

            match simulator.get_designs() {
                DesignSelectionTree::Libraries(libraries) => {
                    for library in libraries {
                        for entity in library.entities {
                            if entity.architectures.is_empty() {
                                designs.push((
                                    SimulatorDesign::Module {
                                        module: entity.name,
                                    },
                                    simulator_name.clone(),
                                ));
                            } else {
                                for architecture in entity.architectures {
                                    designs.push((
                                        SimulatorDesign::Architecture {
                                            library: library.name.clone(),
                                            entity: entity.name.clone(),
                                            architecture,
                                        },
                                        simulator_name.clone(),
                                    ));
                                }
                            }
                        }
                    }
                }
                DesignSelectionTree::Modules(modules) => {
                    for module in modules {
                        designs.push((SimulatorDesign::Module { module }, simulator_name.clone()));
                    }
                }
                DesignSelectionTree::Empty => {}
            }
        }

        designs
    }
}

/// Common interface for simulator backends used by Surfer.
pub trait Simulator: Send + Sync {
    /// Create a new instance of the simulator with an optional working directory.
    fn new(workdir: Option<&Utf8PathBuf>) -> Self
    where
        Self: Sized;

    /// Set the working directory for the simulator, which may affect where it looks for design files and plugins.
    fn set_workdir(&mut self, workdir: Option<&Utf8PathBuf>);

    /// Get the current working directory of the simulator.
    fn workdir(&self) -> Utf8PathBuf;

    /// Get the name of the simulator.
    fn name(&self) -> &str;

    /// Check if the simulator is available for use (e.g., required binaries are in PATH).
    fn is_available(&self) -> bool;

    /// Get a human-readable filter string and list of extensions for file selection dialogs.
    fn compile_filter(&self) -> &'static FileFilter;

    /// Get the expected plugin file name stem for the simulator.
    fn plugin_stem(&self) -> &str;

    /// Get the full plugin file name for the simulator, including platform-specific prefixes and suffixes.
    fn plugin_path(&self) -> Option<Utf8PathBuf> {
        let plugin_name = self.plugin_name();
        let workdir = self.workdir();

        if let Some(path) = plugin_path_from_env(&plugin_name)
            && plugin_has_expected_fingerprint(&path)
        {
            return Some(path);
        }

        info!(
            "Searching for plugin '{plugin_name}'. Set {PLUGIN_ENV_VAR} to a plugin file path or directory to override lookup."
        );

        if let Some(path) = find_plugin_in_dirs(&plugin_candidate_dirs(&workdir), &plugin_name)
            && plugin_has_expected_fingerprint(&path)
        {
            return Some(path);
        }

        #[cfg(test)]
        panic!("recursive plugin search reached while running tests");

        #[cfg(not(test))]
        {
            warn!(
                "Cannot find '{plugin_name}' in standard locations. Doing a recursive search. Set {PLUGIN_ENV_VAR} to a plugin file path or directory to override lookup."
            );
            plugin_search_roots(&workdir).into_iter().find_map(|root| {
                find_plugin_path(root.as_path(), &plugin_name)
                    .filter(plugin_has_expected_fingerprint)
            })
        }
    }

    /// Get the full plugin file name for the simulator, including platform-specific prefixes and suffixes.
    fn plugin_name(&self) -> String {
        let prefix = if cfg!(target_os = "windows") {
            ""
        } else {
            "lib"
        };
        format!(
            "{prefix}surfer_{}_plugin.{}",
            self.plugin_stem(),
            std::env::consts::DLL_EXTENSION
        )
    }
}

fn plugin_has_expected_fingerprint(path: &Utf8PathBuf) -> bool {
    info!("Checking plugin fingerprint for '{}'", path);
    let fingerprint = match plugin_fingerprint(path) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            warn!("Cannot read plugin fingerprint from '{}': {error}", path);
            return false;
        }
    };

    if fingerprint != SURFER_PLUGIN_FINGERPRINT_VALUE {
        warn!(
            "Plugin '{}' fingerprint mismatch: expected {:02x?}, found {:02x?}",
            path, SURFER_PLUGIN_FINGERPRINT_VALUE, fingerprint
        );
        return false;
    }

    true
}

fn plugin_fingerprint(path: &Utf8PathBuf) -> Result<[u8; 32], String> {
    let data = std::fs::read(path.as_std_path()).map_err(|error| error.to_string())?;
    let object = object::File::parse(data.as_slice()).map_err(|error| error.to_string())?;
    let symbol_names = ["SURFER_PLUGIN_FINGERPRINT", "_SURFER_PLUGIN_FINGERPRINT"];
    let address = if let Some(symbol) = symbol_names
        .iter()
        .find_map(|name| object.symbol_by_name(name))
    {
        symbol.address()
    } else {
        object
            .exports()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|export| {
                symbol_names
                    .iter()
                    .any(|name| export.name() == name.as_bytes())
            })
            .map(|export| export.address())
            .ok_or_else(|| "missing SURFER_PLUGIN_FINGERPRINT symbol".to_string())?
    };
    let section = object
        .sections()
        .find(|section| {
            section.address() <= address
                && address
                    .checked_sub(section.address())
                    .and_then(|offset| {
                        offset.checked_add(SURFER_PLUGIN_FINGERPRINT_VALUE.len() as u64)
                    })
                    .is_some_and(|end| end <= section.size())
        })
        .ok_or_else(|| "fingerprint symbol is not in a section".to_string())?;
    let section_data = section.data().map_err(|error| error.to_string())?;
    let offset = address
        .checked_sub(section.address())
        .ok_or_else(|| "fingerprint symbol is outside its section".to_string())?;
    let offset = usize::try_from(offset).map_err(|_| "fingerprint offset is too large")?;
    let end = offset
        .checked_add(SURFER_PLUGIN_FINGERPRINT_VALUE.len())
        .ok_or_else(|| "fingerprint size is too large".to_string())?;
    let fingerprint_data = section_data
        .get(offset..end)
        .ok_or_else(|| "fingerprint symbol is truncated".to_string())?;
    fingerprint_data
        .try_into()
        .map_err(|_| "fingerprint has an unexpected size".to_string())
}

pub trait TwoStageSimulator: Simulator {
    /// Get the design hierarchy for the simulator's working directory.
    fn get_designs(&self) -> DesignSelectionTree;

    /// Compile the provided design files.
    fn compile(&self, files: &[Utf8PathBuf]) -> Result<ExitStatus, String>;

    /// Start a simulator process for the selected design.
    ///
    /// Implementations may reject design variants they cannot execute directly.
    fn run(
        &self,
        design: SimulatorDesign,
        port: Option<u16>,
        debug: bool,
    ) -> std::io::Result<Child>;
}

pub trait OneStageSimulator: Simulator {
    /// Compile the provided design files and start the simulator.
    fn compile_and_run(
        &self,
        files: Vec<Utf8PathBuf>,
        port: Option<u16>,
        debug: bool,
    ) -> std::io::Result<Child>;
}

/// Get the plugin path from the plugin override environment variable, if set, and check if it points to a valid file or directory containing the plugin.
fn plugin_path_from_env(plugin_name: &str) -> Option<Utf8PathBuf> {
    let path = Utf8PathBuf::from_path_buf(std::env::var_os(PLUGIN_ENV_VAR)?.into()).ok()?;

    if path.is_file() {
        return Some(path);
    }

    if path.is_dir() {
        let candidate = path.join(plugin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Search for the plugin file in the given directories, returning the first match found.
fn find_plugin_in_dirs(dirs: &[Utf8PathBuf], plugin_name: &str) -> Option<Utf8PathBuf> {
    for dir in dirs {
        let candidate = dir.join(plugin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Return a list of directories to search for the plugin, starting with the working directory and including common installation paths.
fn plugin_candidate_dirs(workdir: &Utf8PathBuf) -> Vec<Utf8PathBuf> {
    let mut dirs = Vec::new();

    // Add the platform dependent data directory
    if let Some(base_dir) = directories::BaseDirs::new()
        && let Some(base_dir) = Utf8Path::from_path(base_dir.data_dir())
    {
        dirs.push(base_dir.to_path_buf());
    }

    // Add the CARGO_TARGET_DIR if set, along with its common subdirectories.
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR")
        && let Ok(target) = Utf8PathBuf::from_path_buf(target_dir.into())
    {
        dirs.push(target.clone());
        dirs.push(target.join("debug"));
        dirs.push(target.join("release"));
    }

    // Add the working directory and its common build subdirectories to the search paths.
    let workdir_path = workdir.clone();
    dirs.push(workdir_path.clone());
    dirs.push(workdir_path.join("target").join("debug"));
    dirs.push(workdir_path.join("target").join("release"));

    // Add the directory of the currently running executable.
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
        && let Some(exe_dir) = Utf8Path::from_path(exe_dir)
    {
        dirs.push(exe_dir.to_path_buf());
    }

    // Remove duplicates and paths that do not exist while preserving order.
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()) && dir.exists());

    dirs
}

/// Recursively search for the plugin file in the given directory and its subdirectories.
#[cfg(not(test))]
fn find_plugin_path(root: &Utf8Path, plugin_name: &str) -> Option<Utf8PathBuf> {
    let entries = fs::read_dir(root).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();

        if path.file_name().and_then(|name| name.to_str()) == Some(plugin_name) {
            if let Ok(path) = Utf8PathBuf::from_path_buf(path) {
                return Some(path);
            }
        } else if path.is_dir()
            && let Ok(path) = Utf8PathBuf::from_path_buf(path)
            && let Some(found) = find_plugin_path(path.as_path(), plugin_name)
        {
            return Some(found);
        }
    }

    None
}

/// Generate a list of root directories to search for the plugin, starting with the simulator's working directory and including typical installation paths.
#[cfg(not(test))]
fn plugin_search_roots(workdir: &Utf8PathBuf) -> Vec<Utf8PathBuf> {
    let mut roots = Vec::new();

    roots.push(workdir.clone());

    if let Some(parent) = workdir.parent() {
        roots.push(parent.to_path_buf());
    }

    roots.extend(typical_install_roots());

    roots
}

/// Generate a list of typical installation root directories for the current platform.
#[cfg(not(test))]
fn typical_install_roots() -> Vec<Utf8PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    {
        roots.extend(
            [
                std::env::var_os("LOCALAPPDATA"),
                std::env::var_os("APPDATA"),
                std::env::var_os("ProgramFiles"),
                std::env::var_os("ProgramFiles(x86)"),
            ]
            .into_iter()
            .flatten()
            .filter_map(|path| Utf8PathBuf::from_path_buf(path.into()).ok()),
        );
    }

    #[cfg(target_os = "macos")]
    {
        roots.extend(
            [
                std::env::var_os("HOME")
                    .and_then(|home| Utf8PathBuf::from_path_buf(home.into()).ok())
                    .map(|home| home.join("Library/Application Support")),
                Some(Utf8PathBuf::from("/usr/local/lib")),
                Some(Utf8PathBuf::from("/opt/homebrew/lib")),
                Some(Utf8PathBuf::from("/usr/lib")),
            ]
            .into_iter()
            .flatten(),
        );
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        roots.extend(
            [
                std::env::var_os("HOME")
                    .and_then(|home| Utf8PathBuf::from_path_buf(home.into()).ok())
                    .map(|home| home.join(".local/lib")),
                Some(Utf8PathBuf::from("/usr/local/lib")),
                Some(Utf8PathBuf::from("/usr/lib")),
                Some(Utf8PathBuf::from("/lib")),
                Some(Utf8PathBuf::from("/lib64")),
            ]
            .into_iter()
            .flatten(),
        );
    }

    roots
}
