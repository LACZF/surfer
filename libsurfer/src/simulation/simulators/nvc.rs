use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    process::{Child, ExitStatus, Stdio},
};

use crate::simulation::SimulatorDesign;
use crate::simulation::simulator::{DesignEntity, DesignLibrary, DesignSelectionTree, Simulator};
use crate::simulation::simulators::logging::{
    attach_child_output_loggers, log_command_execution, log_command_output,
};
use crate::simulation::simulators::vhdl::vhdl_compile_order;
use camino::{Utf8Path, Utf8PathBuf};
use tracing::{info, warn};

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

    /// NVC uses VPI for its plugin interface, so the expected plugin stem is "vpi".
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
        log_command_execution(self.name(), &command);
        let output = command.output().map_err(|e| e.to_string())?;
        log_command_output(self.name(), &output);
        Ok(output.status)
    }

    fn run(
        &self,
        design: SimulatorDesign,
        port: Option<u16>,
        debug: bool,
    ) -> std::io::Result<Child> {
        let workdir = self.workdir();
        let (entity, elaboration, simulation_file) = match &design {
            SimulatorDesign::Module { .. } => {
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
            let mut command = std::process::Command::new("nvc");
            command.current_dir(&workdir).args(["-e", entity]);
            log_command_execution(self.name(), &command);
            let output = command.output()?;
            log_command_output(self.name(), &output);
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

        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        log_command_execution(self.name(), &command);
        let mut child = command.spawn()?;
        attach_child_output_loggers(&mut child, self.name());
        Ok(child)
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
