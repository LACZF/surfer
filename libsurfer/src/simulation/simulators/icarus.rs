use std::{
    fs,
    process::{Child, ExitStatus, Stdio},
};

use crate::simulation::SimulatorDesign;
use crate::simulation::simulator::{DesignSelectionTree, Simulator};
use crate::simulation::simulators::logging::{
    attach_child_output_loggers, log_command_execution, log_command_output,
};
use camino::Utf8PathBuf;
use tracing::info;

pub struct IcarusSimulator {
    workdir: Option<Utf8PathBuf>,
}

impl IcarusSimulator {
    fn simulation_output_path(&self) -> Utf8PathBuf {
        self.workdir().join("surfer_default.vvp")
    }

    fn simulation_output_path_for_design(&self, design_name: &str) -> Utf8PathBuf {
        self.workdir().join(format!("{design_name}.vvp"))
    }
}

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

    fn name(&self) -> &str {
        "Icarus Verilog"
    }

    fn compile(&self, files: &[Utf8PathBuf]) -> Result<ExitStatus, String> {
        let workdir = self.workdir();
        let output_path = self.simulation_output_path();

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
        if !matches!(design, SimulatorDesign::Module { .. }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cannot run non-entity design via Icarus Verilog runner",
            ));
        }

        let workdir = self.workdir();
        let simulation_file = match design {
            SimulatorDesign::Module { module } => {
                let named_output = self.simulation_output_path_for_design(&module);
                if named_output.exists() {
                    named_output
                } else {
                    self.simulation_output_path()
                }
            }
            SimulatorDesign::Architecture { .. } | SimulatorDesign::ExternalSimulator => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot run non-entity design via Icarus Verilog runner",
                ));
            }
        };
        if !simulation_file.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("compiled simulation file not found: {}", simulation_file),
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
                format!("could not find parent directory for plugin {}", plugin_path),
            )
        })?;
        let plugin_module = plugin_path.file_stem().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not determine module name for plugin {}", plugin_path),
            )
        })?;

        info!(
            "Starting simulation with plugin at {}",
            plugin_path.as_str()
        );
        let mut command = std::process::Command::new("vvp");
        command
            .current_dir(workdir)
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
        attach_child_output_loggers(&mut child, self.name());
        Ok(child)
    }

    fn is_available(&self) -> bool {
        let iverilog_available = std::process::Command::new("iverilog")
            .arg("-V")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        let vvp_available = std::process::Command::new("vvp")
            .arg("-V")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);

        iverilog_available && vvp_available
    }

    fn compile_filter(&self) -> (String, Vec<String>) {
        (
            "Verilog/SystemVerilog-files (*.v, *.sv)".to_string(),
            vec!["v".to_string(), "sv".to_string()],
        )
    }

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

    /// Icarus Verilog uses VPI for its plugin interface, so the expected plugin stem is "vpi".
    fn plugin_stem(&self) -> String {
        "vpi".to_string()
    }

    /// Icarus Verilog prefers to use a plugin with the stem "vpi" for its VPI interface.
    fn plugin_name(&self) -> String {
        "surfer_vpi_plugin.vpi".to_string()
    }
}
