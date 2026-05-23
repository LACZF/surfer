use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
};

use crate::simulation::SimulatorDesign;
use camino::{Utf8Path, Utf8PathBuf};
use tracing::{info, warn};

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
    Entities(Vec<String>),
    #[default]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Represents a design entity in the simulator's working directory.
pub struct DesignEntity {
    pub name: String,
    pub architectures: Vec<String>,
}

/// Manages available simulators and their instantiation.
pub struct SimulatorList {
    simulators: Vec<Box<dyn Simulator>>,
}

impl SimulatorList {
    /// Create a new SimulatorList with all supported simulators.
    pub fn new(workdir: Option<Utf8PathBuf>) -> Self {
        Self {
            simulators: vec![Box::new(NvcSimulator::new(workdir))],
        }
    }

    /// Get a list of available simulators, sorted alphabetically by name.
    pub fn get_available_simulators(&self) -> Vec<&dyn Simulator> {
        let mut available = self
            .simulators
            .iter()
            .map(Box::as_ref)
            .filter(|simulator| simulator.is_available())
            .collect::<Vec<&dyn Simulator>>();

        available.sort_by_key(|simulator| simulator.name().to_ascii_lowercase());
        available
    }

    /// Get a simulator by name, if available.
    pub fn get_simulator_by_name(&self, name: &str) -> Option<&dyn Simulator> {
        self.get_available_simulators()
            .into_iter()
            .find(|simulator| simulator.name() == name)
    }

    /// Create a new instance of a simulator by name, if available.
    pub fn create_simulator_by_name(
        &self,
        name: &str,
        workdir: Option<Utf8PathBuf>,
    ) -> Option<Box<dyn Simulator>> {
        match name {
            "NVC" => Some(Box::new(NvcSimulator::new(workdir))),
            _ => None,
        }
    }

    /// Collect all designs from all available simulators.
    ///
    /// Returns tuples of `(design, simulator_name)`.
    pub(crate) fn all_designs(&self) -> Vec<(SimulatorDesign, String)> {
        let mut designs = Vec::new();

        for simulator in self.get_available_simulators() {
            let simulator_name = simulator.name().to_string();

            match simulator.get_designs() {
                DesignSelectionTree::Libraries(libraries) => {
                    for library in libraries {
                        for entity in library.entities {
                            if entity.architectures.is_empty() {
                                designs.push((
                                    SimulatorDesign::Entity {
                                        entity: entity.name,
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
                DesignSelectionTree::Entities(entities) => {
                    for entity in entities {
                        designs.push((SimulatorDesign::Entity { entity }, simulator_name.clone()));
                    }
                }
                DesignSelectionTree::Empty => {}
            }
        }

        designs
    }
}

/// Return a list of available simulators
pub fn get_simulator_list(workdir: Option<Utf8PathBuf>) -> SimulatorList {
    SimulatorList::new(workdir)
}

/// Common interface for simulator backends used by Surfer.
pub trait Simulator: Send + Sync {
    /// Create a new instance of the simulator with an optional working directory.
    fn new(workdir: Option<Utf8PathBuf>) -> Self
    where
        Self: Sized;
    /// Set the working directory for the simulator, which may affect where it looks for design files and plugins.
    fn set_workdir(&mut self, workdir: Option<Utf8PathBuf>);
    /// Get the current working directory of the simulator.
    fn workdir(&self) -> Utf8PathBuf;
    /// Get the name of the simulator.
    fn name(&self) -> &str;

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
    /// Check if the simulator is available for use (e.g., required binaries are in PATH).
    fn is_available(&self) -> bool;
    /// Get a human-readable filter string and list of extensions for file selection dialogs.
    fn compile_filter(&self) -> (String, Vec<String>);
    /// Get the design hierarchy for the simulator's working directory.
    fn get_designs(&self) -> DesignSelectionTree;
    fn plugin_stem(&self) -> String;
    fn plugin_path(&self) -> Option<Utf8PathBuf> {
        let plugin_name = self.plugin_name();
        let workdir = self.workdir();

        plugin_search_roots(&workdir)
            .into_iter()
            .find_map(|root| find_plugin_path(root.as_path(), &plugin_name))
    }
    fn plugin_name(&self) -> String {
        let prefix = if cfg!(target_os = "windows") {
            ""
        } else {
            "lib"
        };
        format!(
            "{}surfer_{}_plugin.{}",
            prefix,
            self.plugin_stem(),
            std::env::consts::DLL_EXTENSION
        )
    }
}

/// Recursively search for the plugin file in the given directory and its subdirectories.
fn find_plugin_path(root: &Path, plugin_name: &str) -> Option<Utf8PathBuf> {
    let entries = fs::read_dir(root).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();

        if path.file_name().and_then(|name| name.to_str()) == Some(plugin_name) {
            if let Ok(path) = Utf8PathBuf::from_path_buf(path) {
                return Some(path);
            }
        } else if path.is_dir()
            && let Some(found) = find_plugin_path(&path, plugin_name)
        {
            return Some(found);
        }
    }

    None
}

/// Generate a list of root directories to search for the plugin, starting with the simulator's working directory and including typical installation paths.
fn plugin_search_roots(workdir: &Utf8PathBuf) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    roots.push(PathBuf::from(workdir.as_str()));

    if let Some(parent) = workdir.parent() {
        roots.push(parent.to_path_buf().into());
    }

    roots.extend(typical_install_roots());

    roots
}

/// Generate a list of typical installation root directories for the current platform.
fn typical_install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    {
        roots.extend(
            [
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
                std::env::var_os("APPDATA").map(PathBuf::from),
                std::env::var_os("ProgramFiles").map(PathBuf::from),
                std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
            ]
            .into_iter()
            .flatten(),
        );
    }

    #[cfg(target_os = "macos")]
    {
        roots.extend(
            [
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join("Library/Application Support")),
                Some(PathBuf::from("/usr/local/lib")),
                Some(PathBuf::from("/opt/homebrew/lib")),
                Some(PathBuf::from("/usr/lib")),
            ]
            .into_iter()
            .flatten(),
        );
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        roots.extend(
            [
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/lib")),
                Some(PathBuf::from("/usr/local/lib")),
                Some(PathBuf::from("/usr/lib")),
                Some(PathBuf::from("/lib")),
                Some(PathBuf::from("/lib64")),
                Some(PathBuf::from("/opt")),
            ]
            .into_iter()
            .flatten(),
        );
    }

    roots
}

pub struct NvcSimulator {
    workdir: Option<Utf8PathBuf>,
}

impl Simulator for NvcSimulator {
    fn new(workdir: Option<Utf8PathBuf>) -> Self {
        Self { workdir }
    }

    fn set_workdir(&mut self, workdir: Option<Utf8PathBuf>) {
        self.workdir = workdir;
    }

    fn workdir(&self) -> Utf8PathBuf {
        self.workdir.clone().unwrap_or_else(|| {
            Utf8PathBuf::from_path_buf(
                std::env::current_dir().expect("failed to get current working directory"),
            )
            .expect("current working directory is not valid UTF-8")
        })
    }

    fn name(&self) -> &str {
        "NVC"
    }

    fn plugin_stem(&self) -> String {
        "vhpi".to_string()
    }

    fn get_designs(&self) -> DesignSelectionTree {
        fn split_library_and_entity(stem: &str) -> Option<(&str, &str)> {
            stem.split_once('.')
                .filter(|(library, entity)| !library.is_empty() && !entity.is_empty())
        }

        fn split_elab_stem(stem: &str) -> Option<(&str, &str, &str)> {
            let (design_stem, architecture) = stem
                .split_once('-')
                .filter(|(_, architecture)| !architecture.is_empty())?;
            let (library, entity) = split_library_and_entity(design_stem)?;
            Some((library, entity, architecture))
        }

        let workdir = self.workdir().join("work");
        let mut architecture_files: Vec<(String, String, String)> = Vec::new();
        let mut designs: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();

        if let Ok(entries) = std::fs::read_dir(workdir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                if let Some(file_name) = path.file_name().and_then(|s| s.to_str())
                    && let Some((library, entity, architecture)) = split_elab_stem(file_name)
                {
                    architecture_files.push((
                        library.to_string(),
                        entity.to_string(),
                        architecture.to_string(),
                    ));
                }
            }
        }

        for (library, entity, architecture) in architecture_files {
            designs
                .entry(library)
                .or_default()
                .entry(entity)
                .or_default()
                .insert(architecture);
        }

        let libraries = designs
            .into_iter()
            .map(|(library, entities)| DesignLibrary {
                name: library,
                entities: entities
                    .into_iter()
                    .map(|(entity, architectures)| DesignEntity {
                        name: entity,
                        architectures: architectures.into_iter().collect(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        if libraries.is_empty() {
            DesignSelectionTree::Empty
        } else {
            DesignSelectionTree::Libraries(libraries)
        }
    }

    fn compile_filter(&self) -> (String, Vec<String>) {
        (
            "VHDL-files (*.vhd, *.vhdl)".to_string(),
            vec!["vhd".to_string(), "vhdl".to_string()],
        )
    }

    fn compile(&self, files: &[Utf8PathBuf]) -> Result<ExitStatus, String> {
        let workdir = self.workdir();
        let mut command = std::process::Command::new("nvc");
        command.current_dir(workdir);
        match vhdl_compile_order(files) {
            Ok(order) => {
                for file in order {
                    info!("Compiling {}", file.as_str());
                    command.arg("-a").arg(file.as_str());
                }
            }
            Err(cycle) => {
                warn!("Cyclic dependency detected among: {:?}", cycle);
                for file in files {
                    info!("Compiling {}", file.as_str());
                    command.arg("-a").arg(file.as_str());
                }
            }
        }
        command.status().map_err(|e| e.to_string())
    }

    fn run(
        &self,
        design: SimulatorDesign,
        port: Option<u16>,
        debug: bool,
    ) -> std::io::Result<Child> {
        let workdir = self.workdir();
        let (entity, elaboration, simulation_file) = match &design {
            SimulatorDesign::Entity { .. } => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot run entity-only design via NVC runner",
                ));
            }
            SimulatorDesign::Architecture {
                library,
                entity,
                architecture,
            } => {
                let elaboration = self.elaboration_path(library, entity);
                let sim_file =
                    Some(self.simulation_file_path_for_architecture(library, entity, architecture));
                (entity, elaboration, sim_file)
            }
            SimulatorDesign::ExternalSimulator => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot run external simulator via NVC runner",
                ));
            }
        };

        if self.should_elaborate(&elaboration, simulation_file.as_ref()) {
            info!("Elaborating {entity}");
            std::process::Command::new("nvc")
                .current_dir(&workdir)
                .args(["-e", entity])
                .status()?;
        } else {
            info!("Skipping elaboration for {entity}; existing elaboration is up-to-date");
        }

        let plugin_path = self.plugin_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not find {}", self.plugin_name()),
            )
        })?;

        info!(
            "Starting simulation with plugin at {}",
            plugin_path.as_str()
        );
        let mut command = std::process::Command::new("nvc");
        command.current_dir(&workdir).arg("-r");
        if debug {
            command.arg("--vhpi-debug");
        }
        command
            .arg(format!("--load={}", plugin_path.as_str()))
            .arg(entity);

        if let Some(port) = port {
            command.arg(format!("+{}", port));
        }

        command.spawn()
    }

    fn is_available(&self) -> bool {
        std::process::Command::new("nvc")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

impl NvcSimulator {
    /// Returns the expected NVC simulation artifact path for an architecture design.
    fn simulation_file_path_for_architecture(
        &self,
        library: &str,
        entity: &str,
        architecture: &str,
    ) -> Utf8PathBuf {
        self.workdir()
            .join("work")
            .join(format!("{library}.{entity}-{architecture}"))
    }

    fn elaboration_path(&self, library: &str, entity: &str) -> Utf8PathBuf {
        self.workdir()
            .join("work")
            .join(format!("{library}.{entity}.elab"))
    }

    /// Decide whether the design should be elaborated.
    ///
    /// Elaboration is required when the `.elab` file is missing, unreadable,
    /// or older than the simulation artifact.
    fn should_elaborate(
        &self,
        elaboration: &Utf8Path,
        simulation_file: Option<&Utf8PathBuf>,
    ) -> bool {
        let Ok(elab_meta) = fs::metadata(elaboration) else {
            return true;
        };
        let Ok(elab_mtime) = elab_meta.modified() else {
            return true;
        };

        let Some(simulation_file) = simulation_file else {
            return false;
        };
        let Ok(sim_meta) = fs::metadata(simulation_file) else {
            return true;
        };
        let Ok(sim_mtime) = sim_meta.modified() else {
            return true;
        };

        elab_mtime < sim_mtime
    }
}

/// Extract dependencies from a VHDL file by scanning for:
/// `use <library>.<package>`, component declarations/instantiations,
/// and direct entity instantiations.
fn extract_vhdl_dependencies(path: &Utf8Path) -> HashSet<String> {
    let content = fs::read_to_string(path).unwrap_or_default().to_lowercase();
    let mut deps = HashSet::new();
    let mut declared_components = HashSet::new();

    for line in content.lines() {
        let line = line.split("--").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        // Capture: use work.pkg_name.all  or  use work.pkg_name
        if line.starts_with("use ") {
            let parts: Vec<&str> = line.trim_end_matches(';').split('.').collect();
            // parts[0] = "use work", parts[1] = package_name
            if parts.len() >= 2 {
                let library = parts[0].trim_start_matches("use").trim();
                let package = parts[1];
                if library == "work" {
                    deps.insert(package.to_string());
                }
            }
            continue;
        }

        // Capture: component foo is
        if let Some(rest) = line.strip_prefix("component ") {
            if !line.starts_with("end component") {
                let component_name = rest
                    .split_whitespace()
                    .next()
                    .map(|name| name.trim_end_matches(';'));
                if let Some(component_name) = component_name
                    && !component_name.is_empty()
                    && component_name != "is"
                {
                    declared_components.insert(component_name.to_string());
                }
            }
            continue;
        }

        // Capture instantiations: label: entity work.foo(...)  OR  label: foo ...
        if let Some((_, rhs)) = line.split_once(':') {
            let rhs = rhs.trim();

            if let Some(rest) = rhs.strip_prefix("entity ") {
                if let Some(designator) = rest.split_whitespace().next() {
                    let designator = designator
                        .trim_end_matches(';')
                        .split('(')
                        .next()
                        .unwrap_or(designator);

                    if let Some((library, name)) = designator.split_once('.') {
                        if library == "work" && !name.is_empty() {
                            deps.insert(name.to_string());
                        }
                    } else if !designator.is_empty() {
                        deps.insert(designator.to_string());
                    }
                }
                continue;
            }

            if let Some(component_name) = rhs.split_whitespace().next() {
                let component_name = component_name.trim_end_matches(';');
                if declared_components.contains(component_name) {
                    deps.insert(component_name.to_string());
                }
            }
        }
    }
    deps
}

/// Extract the "provided name" of a VHDL file — the package, entity, or architecture name.
fn extract_vhdl_provided_name(path: &Utf8Path) -> Option<String> {
    let content = fs::read_to_string(path).unwrap_or_default().to_lowercase();
    for line in content.lines() {
        let line = line.trim();
        for keyword in &["package ", "entity "] {
            if line.starts_with(keyword) && !line.contains("body") {
                let rest = line[keyword.len()..].trim();
                let name = rest.split_whitespace().next()?;
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Topological sort using Kahn's algorithm.
/// Returns sorted file list or an error listing any cycle members.
fn topological_sort(
    files: &[Utf8PathBuf],
    deps: &HashMap<Utf8PathBuf, HashSet<Utf8PathBuf>>, // file -> set of files it depends on
) -> Result<Vec<Utf8PathBuf>, Vec<Utf8PathBuf>> {
    // Build in-degree map and adjacency list
    let mut in_degree: HashMap<Utf8PathBuf, usize> =
        files.iter().cloned().map(|f| (f, 0)).collect();
    let mut adj: HashMap<Utf8PathBuf, Vec<Utf8PathBuf>> =
        files.iter().cloned().map(|f| (f, vec![])).collect();

    for file in files {
        if let Some(file_deps) = deps.get(file) {
            for dep in file_deps {
                // dep must come before file
                if let Some(neighbors) = adj.get_mut(dep) {
                    neighbors.push(file.clone());
                }
                *in_degree.entry(file.clone()).or_insert(0) += 1;
            }
        }
    }

    // Start with nodes that have no dependencies
    let mut queue: VecDeque<Utf8PathBuf> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(f, _)| f.clone())
        .collect();

    let mut sorted = Vec::new();
    while let Some(node) = queue.pop_front() {
        sorted.push(node.clone());
        if let Some(neighbors) = adj.get(&node) {
            for neighbor in neighbors {
                let deg = in_degree
                    .get_mut(neighbor)
                    .expect("neighbor must exist in in_degree");
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(neighbor.clone());
                }
            }
        }
    }

    if sorted.len() == files.len() {
        Ok(sorted)
    } else {
        // Return files still stuck in a cycle
        let sorted_set: HashSet<&Utf8PathBuf> = sorted.iter().collect();
        let cycle_members = files
            .iter()
            .filter(|f| !sorted_set.contains(f))
            .cloned()
            .collect();
        Err(cycle_members)
    }
}

/// Determine a valid compilation order for VHDL files based on their dependencies.
fn vhdl_compile_order(files: &[Utf8PathBuf]) -> Result<Vec<Utf8PathBuf>, Vec<Utf8PathBuf>> {
    // Map: provided_name -> file_path
    let mut name_to_file: HashMap<String, Utf8PathBuf> = HashMap::new();
    for file in files {
        if let Some(name) = extract_vhdl_provided_name(file.as_path()) {
            name_to_file.insert(name, file.clone());
        }
    }

    // Map: file_path -> set of file_paths it depends on
    let mut file_deps: HashMap<Utf8PathBuf, HashSet<Utf8PathBuf>> = HashMap::new();
    for file in files {
        let dep_names = extract_vhdl_dependencies(file.as_path());
        let dep_files: HashSet<Utf8PathBuf> = dep_names
            .iter()
            .filter_map(|name| name_to_file.get(name).cloned())
            .collect();
        file_deps.insert(file.clone(), dep_files);
    }

    topological_sort(files, &file_deps)
}
