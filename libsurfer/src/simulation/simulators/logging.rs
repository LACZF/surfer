use std::{
    io::{BufRead, BufReader},
    process::{Child, Output},
    thread,
};

use crate::simulation::command_output::{CommandOutputStream, record_command_output};

pub(crate) fn log_command_output(simulator: &str, output: &Output) {
    if !output.stdout.is_empty() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if !line.trim().is_empty() {
                record_command_output(simulator, CommandOutputStream::Stdout, line);
            }
        }
    }

    if !output.stderr.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr.lines() {
            if !line.trim().is_empty() {
                record_command_output(simulator, CommandOutputStream::Stderr, line);
            }
        }
    }
}

pub(crate) fn log_command_execution(simulator: &str, command: &std::process::Command) {
    let command_line = format_command_line(command);
    record_command_output(simulator, CommandOutputStream::Command, &command_line);
}

fn format_command_line(command: &std::process::Command) -> String {
    let mut parts = Vec::new();
    parts.push(format_command_part(
        command.get_program().to_string_lossy().as_ref(),
    ));
    for arg in command.get_args() {
        parts.push(format_command_part(arg.to_string_lossy().as_ref()));
    }
    parts.join(" ")
}

fn format_command_part(part: &str) -> String {
    if part.chars().any(char::is_whitespace) {
        format!("\"{}\"", part)
    } else {
        part.to_string()
    }
}

pub(crate) fn attach_child_output_loggers(child: &mut Child, simulator: &str) {
    let simulator = simulator.to_string();

    if let Some(stdout) = child.stdout.take() {
        let simulator = simulator.clone();
        // Cannot use crate::spawn! here because we need to spawn a thread to read the output, and spawn! is async.
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    record_command_output(&simulator, CommandOutputStream::Stdout, &line);
                }
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        // Cannot use crate::spawn! here because we need to spawn a thread to read the output, and spawn! is async.
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    record_command_output(&simulator, CommandOutputStream::Stderr, &line);
                }
            }
        });
    }
}

#[cfg(test)]
mod command_format_tests {
    use super::{format_command_line, format_command_part};

    #[test]
    fn format_command_part_windows_paths_leaves_plain_tokens_unquoted() {
        assert_eq!(format_command_part("nvc"), "nvc");
        assert_eq!(
            format_command_part("C:\\tools\\nvc.exe"),
            "C:\\tools\\nvc.exe"
        );
        assert_eq!(
            format_command_part("--load=C:\\work\\surfer_vhpi_plugin.dll"),
            "--load=C:\\work\\surfer_vhpi_plugin.dll"
        );
    }

    #[test]
    fn format_command_part_quotes_only_when_whitespace_present() {
        assert_eq!(format_command_part("Program Files"), "\"Program Files\"");
        assert_eq!(format_command_part("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn format_command_line_windows_paths_formats_program_and_args() {
        let mut command = std::process::Command::new("C:\\Program Files\\nvc.exe");
        command
            .arg("-a")
            .arg("C:\\project\\top.vhd")
            .arg("--flag")
            .arg("value with space")
            .arg("--load=C:\\plugins\\surfer_vhpi_plugin.dll");

        let formatted = format_command_line(&command);
        assert_eq!(
            formatted,
            "\"C:\\Program Files\\nvc.exe\" -a C:\\project\\top.vhd --flag \"value with space\" --load=C:\\plugins\\surfer_vhpi_plugin.dll"
        );
    }

    #[test]
    fn format_command_part_linux_macos_paths() {
        assert_eq!(
            format_command_part("/usr/local/bin/nvc"),
            "/usr/local/bin/nvc"
        );
        assert_eq!(
            format_command_part("/opt/homebrew/lib/libsurfer_vhpi_plugin.dylib"),
            "/opt/homebrew/lib/libsurfer_vhpi_plugin.dylib"
        );
        assert_eq!(
            format_command_part("/Applications/My App/bin/nvc"),
            "\"/Applications/My App/bin/nvc\""
        );
    }

    #[test]
    fn format_command_line_linux_macos_style_paths() {
        let mut command = std::process::Command::new("/usr/local/bin/nvc");
        command
            .arg("--load=/opt/homebrew/lib/libsurfer_vhpi_plugin.dylib")
            .arg("--workdir")
            .arg("/Users/alice/My Project")
            .arg("top_entity");

        let formatted = format_command_line(&command);
        assert_eq!(
            formatted,
            "/usr/local/bin/nvc --load=/opt/homebrew/lib/libsurfer_vhpi_plugin.dylib --workdir \"/Users/alice/My Project\" top_entity"
        );
    }
}
