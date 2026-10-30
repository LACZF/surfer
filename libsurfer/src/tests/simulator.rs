// tests for simulator plugins

use super::snapshot::render_and_compare;
use crate::SystemState;
use crate::displayed_item_tree::VisibleItemIndex;
use crate::message::{Message, MessageTarget};
use crate::simulation::simulator::{Simulator, TwoStageSimulator};
use crate::simulation::simulators::{IcarusSimulator, NvcSimulator};
use crate::simulation::{DesignSelectionTree, SimulatorDesign};
use crate::wave_container::{ScopeRef, ScopeRefExt};
use crate::wave_data::ScopeType;
use crate::wave_source::WaveSource;
use camino::Utf8PathBuf;
use std::fs;
use std::net::TcpListener;
use std::process::Command;
use std::sync::OnceLock;
use tempfile::{Builder, TempDir};

static PLUGIN_BUILD_STATUS: OnceLock<Result<Utf8PathBuf, String>> = OnceLock::new();
static VPI_PLUGIN_BUILD_STATUS: OnceLock<Result<Utf8PathBuf, String>> = OnceLock::new();

fn plugin_target_dir(project_root: &Utf8PathBuf) -> Utf8PathBuf {
    project_root.join("target").join("simulator-plugin-tests")
}

fn plugin_build_dir(project_root: &Utf8PathBuf) -> Utf8PathBuf {
    plugin_target_dir(project_root).join("debug")
}

fn ensure_vhpi_plugin_built(project_root: &Utf8PathBuf) -> Utf8PathBuf {
    let result = PLUGIN_BUILD_STATUS.get_or_init(|| {
        let status = Command::new("cargo")
            .current_dir(project_root.as_std_path())
            .args(["build", "-p", "surfer-vhpi-plugin", "--target-dir"])
            .arg(plugin_target_dir(project_root).as_std_path())
            .status()
            .map_err(|e| format!("failed to run cargo build for surfer-vhpi-plugin: {e}"))?;

        if !status.success() {
            return Err(format!(
                "building surfer-vhpi-plugin failed with status {status}"
            ));
        }

        let target_dir = plugin_target_dir(project_root).join("debug");
        let mut stack = vec![target_dir.as_std_path().to_path_buf()];
        let dll_extension = std::env::consts::DLL_EXTENSION;
        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir)
                .map_err(|e| format!("failed to scan build output in {}: {e}", dir.display()))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if (file_name.starts_with("surfer_vhpi_plugin")
                    || file_name.starts_with("libsurfer_vhpi_plugin"))
                    && file_name.ends_with(dll_extension)
                {
                    let plugin_path = Utf8PathBuf::from_path_buf(path).map_err(|path| {
                        format!("plugin path is not valid UTF-8: {}", path.display())
                    })?;
                    unsafe {
                        std::env::set_var(
                            "SURFER_PLUGIN_TEST_PATH",
                            plugin_build_dir(project_root),
                        );
                    }
                    return Ok(plugin_path);
                }
            }
        }

        Err(format!(
            "could not find built surfer-vhpi-plugin artifact under {target_dir}"
        ))
    });

    if let Err(err) = result {
        panic!("{err}");
    }
    result.as_ref().expect("checked above").clone()
}

fn ensure_vhpi_plugin_in_workdir(project_root: &Utf8PathBuf, workdir: &Utf8PathBuf) {
    let plugin_source = ensure_vhpi_plugin_built(project_root);
    let plugin_target = workdir.join(format!(
        "surfer_vhpi_plugin.{}",
        std::env::consts::DLL_EXTENSION
    ));
    fs::copy(plugin_source.as_std_path(), plugin_target.as_std_path())
        .unwrap_or_else(|e| panic!("failed to copy VHPI plugin into workdir {workdir}: {e}"));
    assert!(
        NvcSimulator::new(Some(workdir)).plugin_path().is_some(),
        "no valid VHPI plugin with the expected fingerprint found."
    );
}

fn ensure_vpi_plugin_built(project_root: &Utf8PathBuf) -> Utf8PathBuf {
    let result = VPI_PLUGIN_BUILD_STATUS.get_or_init(|| {
        let plugin_cache_path = plugin_build_dir(project_root).join("surfer_vpi_plugin.vpi");

        let status = Command::new("cargo")
            .current_dir(project_root.as_std_path())
            .args(["build", "-p", "surfer-vpi-plugin", "--target-dir"])
            .arg(plugin_target_dir(project_root).as_std_path())
            .status()
            .map_err(|e| format!("failed to run cargo build for surfer-vpi-plugin: {e}"))?;

        if !status.success() {
            return Err(format!(
                "building surfer-vpi-plugin failed with status {status}"
            ));
        }

        let target_dir = plugin_target_dir(project_root).join("debug");
        let mut stack = vec![target_dir.as_std_path().to_path_buf()];
        let dll_extension = std::env::consts::DLL_EXTENSION;
        let mut built_artifact = None;

        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir)
                .map_err(|e| format!("failed to scan build output in {}: {e}", dir.display()))?;

            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }

                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };

                let looks_like_plugin = (file_name.starts_with("surfer_vpi_plugin")
                    || file_name.starts_with("libsurfer_vpi_plugin"))
                    && file_name.ends_with(dll_extension);
                if looks_like_plugin {
                    built_artifact = Some(path);
                    break;
                }
            }

            if built_artifact.is_some() {
                break;
            }
        }

        let built_artifact = built_artifact.ok_or_else(|| {
            format!(
                "could not find built surfer-vpi-plugin artifact under {}",
                target_dir
            )
        })?;

        fs::create_dir_all(
            plugin_cache_path
                .parent()
                .expect("plugin cache path must have a parent"),
        )
        .map_err(|e| format!("failed to create plugin cache directory: {e}"))?;

        fs::copy(&built_artifact, &plugin_cache_path).map_err(|e| {
            format!(
                "failed to copy plugin artifact from {} to {}: {e}",
                built_artifact.display(),
                plugin_cache_path
            )
        })?;

        unsafe {
            std::env::set_var("SURFER_PLUGIN_TEST_PATH", plugin_build_dir(project_root));
        }

        Ok(plugin_cache_path)
    });

    match result {
        Ok(path) => path.clone(),
        Err(err) => panic!("{err}"),
    }
}

fn ensure_vpi_plugin_in_workdir(project_root: &Utf8PathBuf, workdir: &Utf8PathBuf) {
    let plugin_source = ensure_vpi_plugin_built(project_root);
    let plugin_target = workdir.join("surfer_vpi_plugin.vpi");

    if !plugin_target.exists() {
        fs::copy(plugin_source.as_std_path(), plugin_target.as_std_path()).unwrap_or_else(|e| {
            panic!("failed to copy Icarus plugin into workdir {}: {e}", workdir)
        });
    }
    assert!(
        IcarusSimulator::new(Some(workdir)).plugin_path().is_some(),
        "no valid VPI plugin with the expected fingerprint found in {workdir}"
    );
}

fn materialize_icarus_design_file(workdir: &Utf8PathBuf, design: &SimulatorDesign) {
    let SimulatorDesign::Module { module } = design else {
        return;
    };

    if module == "surfer_default" {
        return;
    }

    let default_output = workdir.join("surfer_default.vvp");
    let named_output = workdir.join(format!("{module}.vvp"));

    if default_output.exists() && !named_output.exists() {
        fs::copy(default_output.as_std_path(), named_output.as_std_path()).unwrap_or_else(|e| {
            panic!(
                "failed to create named Icarus design file {} from {}: {e}",
                named_output, default_output
            )
        });
    }
}

fn design_exists(designs: &DesignSelectionTree, design: &SimulatorDesign) -> bool {
    match (designs, design) {
        (
            DesignSelectionTree::Libraries(libraries),
            SimulatorDesign::Architecture {
                library,
                entity,
                architecture,
            },
        ) => libraries.iter().any(|lib| {
            lib.name == *library
                && lib.entities.iter().any(|ent| {
                    ent.name == *entity && ent.architectures.iter().any(|arch| arch == architecture)
                })
        }),
        (DesignSelectionTree::Modules(modules), SimulatorDesign::Module { module }) => {
            modules.iter().any(|m| m == module)
        }
        (_, SimulatorDesign::ExternalSimulator) => false,
        _ => false,
    }
}

/// Wait until the simulator hierarchy has been received and installed as wave data.
fn wait_for_simulator_hierarchy(state: &mut SystemState, timeout_s: u64) {
    let load_start = std::time::Instant::now();
    while !state
        .user
        .waves
        .as_ref()
        .is_some_and(|waves| matches!(waves.source, WaveSource::SimulatorPlugin(_)))
    {
        state.handle_async_messages();
        state.handle_batch_commands();
        if load_start.elapsed().as_secs() > timeout_s {
            panic!("Timeout after {timeout_s}s while waiting for simulator hierarchy");
        }
    }
}

fn find_free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .unwrap_or_else(|e| panic!("failed to allocate free local port: {e}"));
    listener
        .local_addr()
        .unwrap_or_else(|e| panic!("failed to read local socket address: {e}"))
        .port()
}

/// Wait until a started simulator run has produced data/time updates and finished.
fn wait_for_simulator_run_finished(state: &mut SystemState, timeout_s: u64) {
    let load_start = std::time::Instant::now();
    let mut seen_running = false;

    loop {
        state.handle_async_messages();
        state.handle_batch_commands();

        seen_running |= state.simulation_running();

        // Consider completion only after we've actually seen the simulation running,
        // then wait for waveform data/batch processing to fully converge.
        let run_finished = seen_running && !state.simulation_running();
        let waves_ready = state.waves_fully_loaded() && state.can_start_batch_command();
        if run_finished && waves_ready {
            break;
        }

        // Fallback: if plugin process exits unexpectedly, still require converged wave loading.
        if state
            .simulator_child
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .flatten()
            .is_some()
            && waves_ready
        {
            break;
        }

        if load_start.elapsed().as_secs() > timeout_s {
            panic!(
                "Timeout after {timeout_s}s while waiting for simulator run to finish (seen_running={}, simulation_running={}, waves_fully_loaded={}, can_start_batch_command={})",
                seen_running,
                state.simulation_running(),
                state.waves_fully_loaded(),
                state.can_start_batch_command()
            );
        }
    }
}

/// Wait until waveform content is actually ready to be drawn.
fn wait_for_simulator_waveform_ready(state: &mut SystemState, timeout_s: u64) {
    let load_start = std::time::Instant::now();

    loop {
        state.handle_async_messages();
        state.handle_batch_commands();

        let has_drawable_waveform = state.user.waves.as_ref().is_some_and(|waves| {
            let has_items = !waves.displayed_items.is_empty();
            let has_time_data = waves
                .max_timestamp()
                .is_some_and(|max_timestamp| max_timestamp > 1.into());
            has_items && has_time_data
        });

        if has_drawable_waveform && state.waves_fully_loaded() && state.can_start_batch_command() {
            break;
        }

        if load_start.elapsed().as_secs() > timeout_s {
            panic!(
                "Timeout after {timeout_s}s while waiting for simulator waveform content to be ready"
            );
        }
    }
}

struct SimulatorRunCleanup<'a> {
    state: &'a mut SystemState,
    temp_dir: Option<TempDir>,
}

impl Drop for SimulatorRunCleanup<'_> {
    fn drop(&mut self) {
        // Ensure plugin process is stopped before trying to remove the workdir.
        self.state.update(Message::SimulatorPluginTerminateInstance);

        if let Some(temp_dir) = self.temp_dir.take() {
            let _ = temp_dir.close();
        }
    }
}

/// Compile sources with NVC, start a simulator plugin instance, wait for hierarchy,
/// then apply custom messages and wait for simulator completion.
fn run_with_nvc_simulator(
    compile_files: &[&str],
    design: SimulatorDesign,
    custom_messages: impl Fn() -> Vec<Message>,
    simulator_port: u16,
) -> SystemState {
    let project_root: Utf8PathBuf = project_root::get_project_root()
        .unwrap()
        .try_into()
        .unwrap();
    let temp_dir = Builder::new()
        .prefix("surfer-sim-test-")
        .tempdir_in(project_root.as_std_path())
        .unwrap_or_else(|e| panic!("failed to create temporary test directory: {e}"));
    let workdir: Utf8PathBuf = Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
        .expect("temporary directory path is not valid UTF-8");

    ensure_vhpi_plugin_in_workdir(&project_root, &workdir);

    let nvc = NvcSimulator::new(Some(&workdir));
    assert!(nvc.is_available(), "NVC is not available");

    let files = compile_files
        .iter()
        .map(|f| project_root.join(f))
        .collect::<Vec<_>>();

    match nvc.compile(&files) {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("NVC compile failed with status {status}"),
        Err(err) => panic!("Failed to compile with NVC: {err}"),
    }

    let designs = nvc.get_designs();
    if matches!(designs, DesignSelectionTree::Empty) {
        panic!("No simulator design found after compile");
    }
    assert!(
        design_exists(&designs, &design),
        "requested simulator design {design} was not found after compile"
    );

    let mut state = SystemState::new_default_config().unwrap();
    {
        let cleanup = SimulatorRunCleanup {
            state: &mut state,
            temp_dir: Some(temp_dir),
        };

        cleanup.state.simulator_design = Some(design.clone());
        cleanup.state.simulator_debug = true;

        let child = nvc
            .run(design.clone(), Some(simulator_port), false)
            .unwrap_or_else(|e| panic!("failed to run NVC simulator: {e}"));

        cleanup
            .state
            .update(Message::SimulatorPluginInstanceStarted(child));
        cleanup.state.add_batch_messages(vec![
            Message::SimulatorPluginSetPort(simulator_port),
            Message::SimulatorPluginConnectTo,
            Message::SetMenuVisible(false),
            Message::SetToolbarVisible(false),
            Message::SetOverviewVisible(false),
        ]);

        // First phase: wait until hierarchy is obtained from the simulator plugin.
        wait_for_simulator_hierarchy(cleanup.state, 20);

        // Second phase: enqueue custom messages as a batch, like the main snapshot framework.
        let custom_messages = custom_messages();
        let has_run_message = custom_messages
            .iter()
            .any(|msg| matches!(msg, Message::SimulatorPluginRunFor(_)));
        cleanup.state.add_batch_messages(custom_messages);

        if has_run_message {
            wait_for_simulator_run_finished(cleanup.state, 30);
        } else {
            panic!(
                "Test did not send a SimulatorPluginRunFor message, so waiting for simulator run to finish is not possible"
            );
        }

        wait_for_simulator_waveform_ready(cleanup.state, 20);
        cleanup.state.update(Message::ZoomToFit { viewport_idx: 0 });
        cleanup.state.update(Message::SetContinuousRedraw(true));

        // Force fresh draw-command generation before snapshot rendering.
        cleanup.state.update(Message::InvalidateDrawCommands);
    }

    state
}

/// Compile sources with Icarus Verilog, copy the plugin into the workdir, start a simulator
/// plugin instance, wait for hierarchy, then apply custom messages and wait for simulator
/// completion.
fn run_with_icarus_simulator(
    compile_files: &[&str],
    design: SimulatorDesign,
    custom_messages: impl Fn() -> Vec<Message>,
    simulator_port: u16,
) -> SystemState {
    let project_root: Utf8PathBuf = project_root::get_project_root()
        .unwrap()
        .try_into()
        .unwrap();

    let temp_dir = Builder::new()
        .prefix("surfer-sim-test-")
        .tempdir_in(project_root.as_std_path())
        .unwrap_or_else(|e| panic!("failed to create temporary test directory: {e}"));
    let workdir: Utf8PathBuf = Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
        .expect("temporary directory path is not valid UTF-8");

    ensure_vpi_plugin_in_workdir(&project_root, &workdir);

    let icarus = IcarusSimulator::new(Some(&workdir));
    assert!(icarus.is_available(), "Icarus Verilog is not available");

    let files = compile_files
        .iter()
        .map(|f| project_root.join(f))
        .collect::<Vec<_>>();

    match icarus.compile(&files) {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("Icarus compile failed with status {status}"),
        Err(err) => panic!("Failed to compile with Icarus Verilog: {err}"),
    }

    materialize_icarus_design_file(&workdir, &design);
    ensure_vpi_plugin_in_workdir(&project_root, &workdir);

    let designs = icarus.get_designs();
    if matches!(designs, DesignSelectionTree::Empty) {
        panic!("No simulator design found after compile");
    }
    assert!(
        design_exists(&designs, &design),
        "requested simulator design {design} was not found after compile"
    );

    let mut state = SystemState::new_default_config().unwrap();
    {
        let cleanup = SimulatorRunCleanup {
            state: &mut state,
            temp_dir: Some(temp_dir),
        };

        cleanup.state.simulator_design = Some(design.clone());
        cleanup.state.simulator_debug = true;

        let child = icarus
            .run(design.clone(), Some(simulator_port), false)
            .unwrap_or_else(|e| panic!("failed to run Icarus Verilog simulator: {e}"));

        cleanup
            .state
            .update(Message::SimulatorPluginInstanceStarted(child));
        cleanup.state.add_batch_messages(vec![
            Message::SimulatorPluginSetPort(simulator_port),
            Message::SimulatorPluginConnectTo,
            Message::SetMenuVisible(false),
            Message::SetToolbarVisible(false),
            Message::SetOverviewVisible(false),
        ]);

        wait_for_simulator_hierarchy(cleanup.state, 20);

        let custom_messages = custom_messages();
        let has_run_message = custom_messages
            .iter()
            .any(|msg| matches!(msg, Message::SimulatorPluginRunFor(_)));
        cleanup.state.add_batch_messages(custom_messages);

        if has_run_message {
            wait_for_simulator_run_finished(cleanup.state, 10);
        } else {
            panic!(
                "Test did not send a SimulatorPluginRunFor message, so waiting for simulator run to finish is not possible"
            );
        }

        wait_for_simulator_waveform_ready(cleanup.state, 10);
        cleanup.state.update(Message::ZoomToFit { viewport_idx: 0 });
        cleanup.state.update(Message::SetContinuousRedraw(true));
        cleanup.state.update(Message::InvalidateDrawCommands);
    }

    state
}

macro_rules! snapshot_ui_simulator_nvc {
    ($name:ident, $design:expr, [$($compile_file:expr),+ $(,)?], [$($msg:expr),* $(,)?]) => {
        #[test]
        fn $name() {
              let _ = tracing_subscriber::fmt()
                .with_test_writer()
                .try_init();
            if !NvcSimulator::new(None).is_available() {
                eprintln!("Skipping simulator snapshot test: NVC is not available");
                return;
            }
            let simulator_port = find_free_port();
            let project_root: Utf8PathBuf =
                project_root::get_project_root().unwrap().try_into().unwrap();
            ensure_vhpi_plugin_built(&project_root);

            let mut test_name = "simulator/".to_string();
            test_name.push_str(stringify!($name));

            let design = $design;

            render_and_compare(&Utf8PathBuf::from(test_name), || {
                run_with_nvc_simulator(&[$($compile_file),+], design.clone(), || vec![$($msg),*], simulator_port)
            })
        }
    };
}

macro_rules! snapshot_ui_simulator_icarus {
    ($name:ident, $design:expr, [$($compile_file:expr),+ $(,)?], [$($msg:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            let _ = tracing_subscriber::fmt()
                .with_test_writer()
                .try_init();
            if !IcarusSimulator::new(None).is_available() {
                eprintln!("Skipping simulator snapshot test: Icarus Verilog is not available");
                return;
            }
            let simulator_port = find_free_port();
            let project_root: Utf8PathBuf =
                project_root::get_project_root().unwrap().try_into().unwrap();
            ensure_vpi_plugin_built(&project_root);

            let mut test_name = "simulator/".to_string();
            test_name.push_str(stringify!($name));

            let design = $design;

            render_and_compare(&Utf8PathBuf::from(test_name), || {
                run_with_icarus_simulator(&[$($compile_file),+], design.clone(), || vec![$($msg),*], simulator_port)
            })
        }
    };
}

snapshot_ui_simulator_nvc!(
    nvc_simple_design_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_SIMPLE".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_simple.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_SIMPLE"
        ])))),
        Message::AddScope(ScopeRef::from_strs(&["TB_SIMPLE"]), false),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_data_types_renders,
    SimulatorDesign::Module {
        module: "tb_data_types".to_string(),
    },
    ["surfer-vpi-plugin/verilog_examples/tb_data_types.v"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_data_types"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_data_types"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_sv_data_types_renders,
    SimulatorDesign::Module {
        module: "tb_data_types".to_string(),
    },
    ["surfer-vpi-plugin/system_verilog_examples/tb_data_types.sv"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_data_types"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_data_types"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_hierarchy_renders,
    SimulatorDesign::Module {
        module: "tb_hierarchy_top".to_string(),
    },
    [
        "surfer-vpi-plugin/verilog_examples/hierarchy/tb_hierarchy_top.v",
        "surfer-vpi-plugin/verilog_examples/hierarchy/tb_hierarchy_mid.v",
        "surfer-vpi-plugin/verilog_examples/hierarchy/tb_hierarchy_leaf.v"
    ],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_hierarchy_top"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_hierarchy_top"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_arrays_renders,
    SimulatorDesign::Module {
        module: "tb_arrays".to_string(),
    },
    ["surfer-vpi-plugin/verilog_examples/tb_arrays.v"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_arrays"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_arrays"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_simple_logic_renders,
    SimulatorDesign::Module {
        module: "tb_simple_logic".to_string(),
    },
    ["surfer-vpi-plugin/verilog_examples/tb_simple_logic.v"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_simple_logic"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_simple_logic"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_simple_logic_run_for_165ns_renders,
    SimulatorDesign::Module {
        module: "tb_simple_logic".to_string(),
    },
    ["surfer-vpi-plugin/verilog_examples/tb_simple_logic.v"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_simple_logic"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_simple_logic"]), true),
        Message::SetDefaultTimeline(false),
        Message::SimulatorPluginRunFor(Some(165_000))
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_simple_logic_run_for_165ns_add_run_renders,
    SimulatorDesign::Module {
        module: "tb_simple_logic".to_string(),
    },
    ["surfer-vpi-plugin/verilog_examples/tb_simple_logic.v"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_simple_logic"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_simple_logic"]), false),
        Message::RemoveVisibleItems(MessageTarget::Explicit(VisibleItemIndex(3))),
        Message::SetDefaultTimeline(false),
        Message::SimulatorPluginRunFor(Some(165_000)),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_simple_logic"]), false),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_icarus!(
    icarus_events_renders,
    SimulatorDesign::Module {
        module: "tb_events".to_string(),
    },
    ["surfer-vpi-plugin/verilog_examples/tb_events.v"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "tb_events"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["tb_events"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_simple_design_run_for_5s_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_SIMPLE".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_simple.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_SIMPLE"
        ])))),
        Message::AddScope(ScopeRef::from_strs(&["TB_SIMPLE"]), false),
        Message::SetDefaultTimeline(false),
        Message::SimulatorPluginRunFor(Some(5_500_000_000_000_000))
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_simple_design_run_for_5s_add_run_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_SIMPLE".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_simple.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_SIMPLE"
        ])))),
        Message::AddScope(ScopeRef::from_strs(&["TB_SIMPLE"]), false),
        Message::SetDefaultTimeline(false),
        Message::RemoveVisibleItems(MessageTarget::Explicit(VisibleItemIndex(1))),
        Message::SimulatorPluginRunFor(Some(5_500_000_000_000_000)),
        Message::AddScope(ScopeRef::from_strs(&["TB_SIMPLE"]), true),
        Message::SimulatorPluginRunFor(None),
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_multifile_counter_design_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_TOP_COUNTER".to_string(),
        architecture: "SIM".to_string(),
    },
    [
        "surfer-vhpi-plugin/test_examples/top_counter.vhdl",
        "surfer-vhpi-plugin/test_examples/tb_top_counter.vhdl",
        "surfer-vhpi-plugin/test_examples/counter_component.vhdl"
    ],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_TOP_COUNTER"
        ])))),
        Message::AddScope(ScopeRef::from_strs(&["TB_TOP_COUNTER"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_arrays_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_ARRAYS".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_arrays.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_ARRAYS"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["TB_ARRAYS"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_enums_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_ENUMS".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_enums.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_ENUMS"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["TB_ENUMS"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_string_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_STRING".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_string.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_STRING"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["TB_STRING"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_complex_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_COMPLEX".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_complex.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_COMPLEX"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["TB_COMPLEX"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

snapshot_ui_simulator_nvc!(
    nvc_physical_time_renders,
    SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_PHYSICAL_TIME".to_string(),
        architecture: "SIM".to_string(),
    },
    ["surfer-vhpi-plugin/test_examples/tb_physical_time.vhdl"],
    [
        Message::SetActiveScope(Some(ScopeType::WaveScope(ScopeRef::from_strs(&[
            "TB_PHYSICAL_TIME"
        ])))),
        Message::AddScopeAsGroup(ScopeRef::from_strs(&["TB_PHYSICAL_TIME"]), true),
        Message::SimulatorPluginRunFor(None)
    ]
);

#[test]
fn nvc_tb_simple_remove_signal_keeps_signal_ref_to_id_and_runs_to_end() {
    if !NvcSimulator::new(None).is_available() {
        eprintln!("Skipping simulator integration test: NVC is not available");
        return;
    }
    let simulator_port = find_free_port();
    let project_root: Utf8PathBuf = project_root::get_project_root()
        .unwrap()
        .try_into()
        .unwrap();
    ensure_vhpi_plugin_built(&project_root);

    // Match snapshot test runtime setup so plugin connection tasks can use Tokio.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let _enter = runtime.enter();
    let _runtime_driver = std::thread::spawn(move || {
        runtime.block_on(async {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
            }
        });
    });

    let temp_dir = Builder::new()
        .prefix("surfer-sim-test-")
        .tempdir_in(project_root.as_std_path())
        .unwrap_or_else(|e| panic!("failed to create temporary test directory: {e}"));
    let workdir: Utf8PathBuf = Utf8PathBuf::from_path_buf(temp_dir.path().to_path_buf())
        .expect("temporary directory path is not valid UTF-8");
    ensure_vhpi_plugin_in_workdir(&project_root, &workdir);

    let nvc = NvcSimulator::new(Some(&workdir));
    let design = SimulatorDesign::Architecture {
        library: "WORK".to_string(),
        entity: "TB_SIMPLE".to_string(),
        architecture: "SIM".to_string(),
    };
    let files = vec![project_root.join("surfer-vhpi-plugin/test_examples/tb_simple.vhdl")];

    match nvc.compile(&files) {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("NVC compile failed with status {status}"),
        Err(err) => panic!("Failed to compile with NVC: {err}"),
    }

    let designs = nvc.get_designs();
    assert!(
        design_exists(&designs, &design),
        "requested simulator design {design} was not found after compile"
    );

    let mut state = SystemState::new_default_config().unwrap();
    {
        let cleanup = SimulatorRunCleanup {
            state: &mut state,
            temp_dir: Some(temp_dir),
        };

        cleanup.state.simulator_design = Some(design.clone());
        cleanup.state.simulator_debug = true;

        let child = nvc
            .run(design, Some(simulator_port), false)
            .unwrap_or_else(|e| panic!("failed to run NVC simulator: {e}"));

        cleanup
            .state
            .update(Message::SimulatorPluginInstanceStarted(child));
        cleanup.state.add_batch_messages(vec![
            Message::SimulatorPluginSetPort(simulator_port),
            Message::SimulatorPluginConnectTo,
            Message::SetMenuVisible(false),
            Message::SetToolbarVisible(false),
            Message::SetOverviewVisible(false),
        ]);

        wait_for_simulator_hierarchy(cleanup.state, 20);
        assert_eq!(
            cleanup.state.signal_ref_to_id.as_ref().map(|m| m.len()),
            Some(4),
            "expected four simulator IDs after hierarchy load"
        );

        cleanup
            .state
            .update(Message::SetActiveScope(Some(ScopeType::WaveScope(
                ScopeRef::from_strs(&["TB_SIMPLE"]),
            ))));
        cleanup.state.update(Message::AddScope(
            ScopeRef::from_strs(&["TB_SIMPLE"]),
            false,
        ));

        cleanup
            .state
            .update(Message::SimulatorPluginRunFor(Some(165_000)));
        wait_for_simulator_run_finished(cleanup.state, 30);

        let remove_vidx = cleanup
            .state
            .user
            .waves
            .as_ref()
            .and_then(|waves| {
                waves
                    .items_tree
                    .iter_visible()
                    .enumerate()
                    .find_map(|(idx, node)| {
                        matches!(
                            waves.displayed_items.get(&node.item_ref),
                            Some(crate::displayed_item::DisplayedItem::Variable(_))
                        )
                        .then_some(crate::displayed_item_tree::VisibleItemIndex(idx))
                    })
            })
            .expect("expected at least one displayed variable to remove");

        cleanup.state.update(Message::RemoveVisibleItems(
            crate::message::MessageTarget::Explicit(remove_vidx),
        ));

        assert_eq!(
            cleanup.state.signal_ref_to_id.as_ref().map(|m| m.len()),
            Some(4),
            "expected simulator ID map to remain stable after removing one signal"
        );

        cleanup.state.update(Message::SimulatorPluginRunFor(None));
        wait_for_simulator_run_finished(cleanup.state, 30);

        assert_eq!(
            cleanup.state.signal_ref_to_id.as_ref().map(|m| m.len()),
            Some(4),
            "expected simulator ID map to remain stable after full simulation run"
        );
    }
}
