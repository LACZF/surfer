use std::process::{Child, Stdio};

use crate::simulation::simulator::{OneStageSimulator, Simulator};
use crate::simulation::simulators::logging::{
    LogLevel, attach_child_output_loggers, log_command_execution,
};
use crate::simulation::{
    command_output::{CommandOutputStream, record_command_output},
    simulators::logging::classify_log_line,
};
use camino::Utf8PathBuf;

#[derive(Debug, Clone)]
pub struct XezimSimulator {
    workdir: Option<Utf8PathBuf>,
}

impl Simulator for XezimSimulator {
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
        "Xezim"
    }

    fn is_available(&self) -> bool {
        std::process::Command::new("xezim")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn compile_filter(&self) -> (String, Vec<String>) {
        (
            "Verilog/SystemVerilog-files (*.v, *.sv)".to_string(),
            vec!["v".to_string(), "sv".to_string()],
        )
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

impl OneStageSimulator for XezimSimulator {
    /// Compile the provided design files and start the simulator.
    fn compile_and_run(
        &self,
        files: Vec<Utf8PathBuf>,
        port: Option<u16>,
        _debug: bool,
    ) -> std::io::Result<Child> {
        let workdir = self.workdir();
        let mut command = std::process::Command::new("xezim");
        command.current_dir(workdir);

        for file in files {
            command.arg(file);
        }

        let plugin_path = self.plugin_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("could not find {}", self.plugin_name()),
            )
        })?;

        command.arg("--vpi-lib").arg(&plugin_path);

        if let Some(port) = port {
            command.arg(format!("+{port}"));
        }

        record_command_output(
            self.name(),
            CommandOutputStream::Log,
            LogLevel::Info,
            &format!("Starting simulation with plugin at {plugin_path}"),
        );

        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        log_command_execution(self.name(), &command);
        let mut child = command.spawn()?;
        attach_child_output_loggers(&mut child, self.name(), classify_log_line);
        Ok(child)
    }
}
