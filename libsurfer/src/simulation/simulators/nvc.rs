use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    process::{Child, ExitStatus, Stdio},
};

use crate::file_dialog::{FileFilter, VHDL_COMPILE_FILTER};
use crate::simulation::simulator::{
    DesignEntity, DesignLibrary, DesignSelectionTree, Simulator, TwoStageSimulator,
};
use crate::simulation::simulators::logging::{
    LogLevel, attach_child_output_loggers, log_command_execution, log_command_output,
};
use crate::simulation::simulators::vhdl::vhdl_compile_order;
use crate::simulation::{
    SimulatorDesign,
    command_output::{CommandOutputStream, record_command_output},
    simulators::logging::classify_log_line,
};
use camino::{Utf8Path, Utf8PathBuf};

#[derive(Debug, Clone)]
pub struct NvcSimulator {
    workdir: Option<Utf8PathBuf>,
}

impl Simulator for NvcSimulator {
    fn new(workdir: Option<&Utf8PathBuf>) -> Self {
        Self {
            workdir: workdir.cloned(),
        }
    }

    fn set_workdir(&mut self, workdir: Option<&Utf8PathBuf>) {
        self.workdir = workdir.cloned();
    }

    fn workdir(&self) -> Utf8PathBuf {
        self.workdir.clone().unwrap_or_else(|| {
            let cwd = std::env::current_dir().expect("failed to get current working directory");
            Utf8PathBuf::from_path_buf(cwd).expect("current working directory is not valid UTF-8")
        })
    }

    fn name(&self) -> &'static str {
        "NVC"
    }

    /// NVC uses VHPI for its plugin interface, so the expected plugin stem is "vhpi".
    fn plugin_stem(&self) -> &'static str {
        "vhpi"
    }

    fn compile_filter(&self) -> &'static FileFilter {
        &VHDL_COMPILE_FILTER
    }

    fn is_available(&self) -> bool {
        which::which("nvc").is_ok()
    }
}

impl TwoStageSimulator for NvcSimulator {
    fn get_designs(&self) -> DesignSelectionTree {
        let workdir = self.workdir().join("work");
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
                    designs
                        .entry(library.to_string())
                        .or_default()
                        .entry(entity.to_string())
                        .or_default()
                        .insert(architecture.to_string());
                }
            }
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

    fn compile(&self, files: &[Utf8PathBuf]) -> Result<ExitStatus, String> {
        let workdir = self.workdir();
        let mut command = std::process::Command::new("nvc");
        command.current_dir(workdir);
        match vhdl_compile_order(files) {
            Ok(order) => {
                for file in order {
                    record_command_output(
                        self.name(),
                        CommandOutputStream::Log,
                        LogLevel::Info,
                        &format!("Compiling {file}"),
                    );
                    command.arg("-a").arg(file.as_str());
                }
            }
            Err(cycle) => {
                record_command_output(
                    self.name(),
                    CommandOutputStream::Log,
                    LogLevel::Warning,
                    &format!("Cyclic dependency detected among: {cycle:?}"),
                );
                for file in files {
                    record_command_output(
                        self.name(),
                        CommandOutputStream::Log,
                        LogLevel::Info,
                        &format!("Compiling {file}"),
                    );
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
            SimulatorDesign::Architecture {
                library,
                entity,
                architecture,
            } => {
                let elaboration = self
                    .workdir()
                    .join("work")
                    .join(format!("{library}.{entity}.elab"));
                let sim_file = self
                    .workdir()
                    .join("work")
                    .join(format!("{library}.{entity}-{architecture}"));
                (entity, elaboration, sim_file)
            }
            SimulatorDesign::Module { .. } | SimulatorDesign::ExternalSimulator => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot run this design via NVC runner",
                ));
            }
        };

        if should_elaborate(&elaboration, &simulation_file) {
            record_command_output(
                self.name(),
                CommandOutputStream::Log,
                LogLevel::Info,
                &format!("Elaborating {entity}"),
            );
            let mut command = std::process::Command::new("nvc");
            command.current_dir(&workdir).args(["-e", entity]);
            log_command_execution(self.name(), &command);
            let output = command.output()?;
            log_command_output(self.name(), &output);
        } else {
            record_command_output(
                self.name(),
                CommandOutputStream::Log,
                LogLevel::Info,
                &format!("Skipping elaboration for {entity}; existing elaboration is up-to-date"),
            );
        }

        let plugin_path = self.plugin_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not find {}", self.plugin_name()),
            )
        })?;

        record_command_output(
            self.name(),
            CommandOutputStream::Log,
            LogLevel::Info,
            &format!("Starting simulation with plugin at {plugin_path}"),
        );
        let mut command = std::process::Command::new("nvc");
        command.current_dir(&workdir).arg("-r");
        if debug {
            command.arg("--vhpi-debug");
        }
        command
            .arg(format!("--load={}", plugin_path.as_str()))
            .arg(entity);
        command.arg("+no-info");
        if debug {
            command.arg("+debug");
        }

        if let Some(port) = port {
            command.arg(format!("+port={port}"));
        }

        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        log_command_execution(self.name(), &command);
        let mut child = command.spawn()?;
        attach_child_output_loggers(&mut child, self.name(), classify_log_line);
        Ok(child)
    }
}

/// Decide whether the design should be elaborated.
///
/// Elaboration is required when the `.elab` file is missing, unreadable,
/// or older than the simulation artifact.
fn should_elaborate(elaboration: &Utf8Path, simulation_file: &Utf8PathBuf) -> bool {
    let Ok(elab_meta) = fs::metadata(elaboration) else {
        return true;
    };
    let Ok(elab_mtime) = elab_meta.modified() else {
        return true;
    };

    let Ok(sim_meta) = fs::metadata(simulation_file) else {
        return true;
    };
    let Ok(sim_mtime) = sim_meta.modified() else {
        return true;
    };

    elab_mtime < sim_mtime
}

/// Split a stem of the form "library.entity-architecture" into its components.
fn split_elab_stem(stem: &str) -> Option<(&str, &str, &str)> {
    let (design_stem, architecture) = stem
        .split_once('-')
        .filter(|(_, architecture)| !architecture.is_empty())?;
    let (library, entity) = design_stem
        .split_once('.')
        .filter(|(library, entity)| !library.is_empty() && !entity.is_empty())?;
    Some((library, entity, architecture))
}
