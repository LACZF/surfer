use std::{
    fs,
    process::{Child, ExitStatus, Stdio},
};

use crate::file_dialog::{FileFilter, VERILOG_COMPILE_FILTER};
use crate::simulation::simulator::{DesignSelectionTree, Simulator, TwoStageSimulator};
use crate::simulation::simulators::logging::{
    LogLevel, attach_child_output_loggers, log_command_execution, log_command_output,
};
use crate::simulation::{
    SimulatorDesign,
    command_output::{CommandOutputStream, record_command_output},
    simulators::logging::classify_log_line,
};
use camino::Utf8PathBuf;

#[derive(Debug, Clone)]
pub struct IcarusSimulator {
    workdir: Option<Utf8PathBuf>,
}

const DEFAULT_SIMULATION_OUTPUT: &str = "surfer_default.vvp";

impl Simulator for IcarusSimulator {
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

    fn name(&self) -> &'static str {
        "Icarus Verilog"
    }

    fn is_available(&self) -> bool {
        let iverilog_available = std::process::Command::new("iverilog")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success());
        let vvp_available = std::process::Command::new("vvp")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success());

        iverilog_available && vvp_available
    }

    fn compile_filter(&self) -> &'static FileFilter {
        &VERILOG_COMPILE_FILTER
    }

    /// Icarus Verilog uses VPI for its plugin interface, so the expected plugin stem is "vpi".
    fn plugin_stem(&self) -> &'static str {
        "vpi"
    }

    /// Icarus Verilog prefers to use a plugin with the stem "vpi" for its VPI interface.
    fn plugin_name(&self) -> String {
        "surfer_vpi_plugin.vpi".to_string()
    }
}

impl TwoStageSimulator for IcarusSimulator {
    fn get_designs(&self) -> DesignSelectionTree {
        let workdir = self.workdir();
        let mut designs = Vec::new();

        if let Ok(entries) = fs::read_dir(&workdir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && let Some(file_name) = path.file_name().and_then(|name| name.to_str())
                    && let Some(design_name) = file_name.strip_suffix(".vvp")
                {
                    designs.push(design_name.to_string());
                }
            }
        }

        if designs.is_empty() {
            DesignSelectionTree::Empty
        } else {
            designs.sort();
            designs.dedup();
            DesignSelectionTree::Modules(designs)
        }
    }

    fn compile(&self, files: &[Utf8PathBuf]) -> Result<ExitStatus, String> {
        let workdir = self.workdir();
        let output_path = workdir.join(DEFAULT_SIMULATION_OUTPUT);

        fs::create_dir_all(output_path.parent().expect("output path has parent"))
            .map_err(|e| e.to_string())?;

        let mut command = std::process::Command::new("iverilog");
        command
            .current_dir(workdir)
            .arg("-g2012")
            .arg("-o")
            .arg(output_path.as_str());

        for file in files {
            command.arg(file.as_str());
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
        let simulation_file = match design {
            SimulatorDesign::Module { module } => {
                let named_output = workdir.join(format!("{module}.vvp"));
                if named_output.exists() {
                    named_output
                } else {
                    workdir.join(DEFAULT_SIMULATION_OUTPUT)
                }
            }
            SimulatorDesign::Architecture { .. } | SimulatorDesign::ExternalSimulator => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot run this design via Icarus Verilog runner",
                ));
            }
        };
        if !simulation_file.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("compiled simulation file not found: {simulation_file}"),
            ));
        }

        let plugin_path = self.plugin_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not find {}", self.plugin_name()),
            )
        })?;
        let plugin_dir = plugin_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not find parent directory for plugin {plugin_path}"),
            )
        })?;
        let plugin_module = plugin_path.file_stem().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not determine module name for plugin {plugin_path}"),
            )
        })?;

        record_command_output(
            self.name(),
            CommandOutputStream::Log,
            LogLevel::Info,
            &format!("Starting simulation with plugin at {plugin_path}"),
        );
        let mut command = std::process::Command::new("vvp");
        command
            .current_dir(workdir)
            .arg("-i")
            .arg("-M")
            .arg(plugin_dir.as_str())
            .arg("-m")
            .arg(plugin_module);
        if debug {
            command.arg("-v");
        }
        command.arg(simulation_file.as_str());

        if let Some(port) = port {
            command.arg(format!("+{port}"));
        }

        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        log_command_execution(self.name(), &command);
        let mut child = command.spawn()?;
        attach_child_output_loggers(&mut child, self.name(), classify_log_line);
        Ok(child)
    }
}
