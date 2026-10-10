#[cfg(not(target_arch = "wasm32"))]
use crate::simulation::DesignSelectionTree;
use crate::{
    SystemState,
    channels::checked_send,
    message::Message,
    simulation::{SimulatorDesign, SimulatorInfo},
    spawn,
    time::time_string,
    wave_container::{SignalId, SimulationStatus, VariableRef, WaveContainer},
    wave_source::{LoadOptions, LoadProgress, LoadProgressStatus, WaveFormat, WaveSource},
    wellen::BodyResult,
};

use ahash::AHashMap;
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use base64::{Engine as _, engine::general_purpose};
#[cfg(not(target_arch = "wasm32"))]
use camino::Utf8PathBuf;
use egui::{Context, RichText};
#[cfg(not(target_arch = "wasm32"))]
use futures::SinkExt;
use num::BigInt;
use std::sync::mpsc::Sender;
use surfer_simulator_protocol::{
    DEFAULT_SIMULATOR_PORT, HierarchyElement, MAX_FRAME_SIZE, ScopeType, SignalEncoding, SimId,
    SimulatorToSurferMessage, SurferToSimulatorMessage, TimeStep, VarDirection, VarIndex, VarType,
    decode_simulator_to_surfer_message, encode_surfer_to_simulator_message,
};
use surfer_time_types::{TimeScale, TimeUnit};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
#[cfg(not(target_arch = "wasm32"))]
use tokio_stream::StreamExt;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::bytes::Bytes;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::{error, info, trace};
use wellen::{Hierarchy, HierarchyBuilder, SignalRef};

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use crate::wasm_api::{next_simulation_proxy_message, notify_host};

impl SystemState {
    pub(crate) fn is_simulation(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .is_some_and(|waves| matches!(waves.source, WaveSource::SimulatorPlugin(_)))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn handle_select_files_to_compile(&mut self, simulator: String) {
        self.simulator = Some(simulator);
        self.open_compile_file_dialog(false);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn handle_select_files_to_compile_and_run(&mut self, simulator: String) {
        self.simulator = Some(simulator);
        self.open_compile_file_dialog(true);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn handle_simulator_plugin_run_with(
        &mut self,
        simulator: String,
        display_window: bool,
    ) -> Option<()> {
        self.simulator = Some(simulator);
        let simulator = self
            .simulators
            .get_simulator_by_name(self.simulator.as_ref()?)?;
        self.design_selection_items = simulator.get_designs();
        self.simulator_design = None;
        self.show_design_selection_window = display_window;
        Some(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn handle_compile_files(&mut self, paths: Vec<Utf8PathBuf>, simulator: String) {
        use crate::simulation::simulator::Sim;

        self.user.show_command_output = true;
        let Some(Sim::TwoStage(simulator)) = self.simulators.get_simulator_by_name(&simulator)
        else {
            return;
        };

        if !simulator.is_available() {
            error!("{} simulator not found, cannot compile", simulator.name());
            return;
        }

        match simulator.compile(&paths) {
            Ok(status) if status.success() => {
                info!("{} compile succeeded", simulator.name());
            }
            Ok(status) => {
                error!("{} compile failed with status {status}", simulator.name());
            }
            Err(err) => {
                error!("Failed to compile with {}: {err}", simulator.name());
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn handle_compile_files_and_run(
        &mut self,
        paths: Vec<Utf8PathBuf>,
        simulator: String,
    ) -> Option<()> {
        use crate::simulation::simulator::Sim;

        self.user.show_command_output = true;
        let workdir = Utf8PathBuf::from_path_buf(std::env::current_dir().unwrap()).unwrap();
        let Some(Sim::OneStage(simulator)) = self
            .simulators
            .create_simulator_by_name(&simulator, Some(&workdir))
        else {
            return None;
        };
        self.user.show_command_output = true;
        let sender = self.channels.msg_sender.clone();
        let simulator_plugin_tx = self.channels.simulator_plugin_tx.clone();
        let old_child = self.simulator_child.take();
        if old_child.is_some()
            && let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::ShuttingDown);
        }
        if let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::Starting);
        }
        let port = self.simulator_port;
        let debug = self.simulator_debug;

        if !simulator.is_available() {
            error!("{} simulator not found, cannot compile", simulator.name());
            return None;
        }

        let paths = paths.clone();

        crate::async_util::perform_work(move || {
            let finish = || checked_send(&sender, Message::SimulatorPluginRestartFinished);

            // Ask the previous plugin process to shut down before forcing it to exit.
            if let Some(mut old) = old_child {
                if let Some(tx) = &simulator_plugin_tx {
                    let _ = tx.send(SurferToSimulatorMessage::Shutdown);
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while old.try_wait().ok().flatten().is_none()
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if old.try_wait().ok().flatten().is_none() {
                    let _ = old.kill();
                    let _ = old.wait();
                }
            }

            // Spawn simulator plugin process
            let child = match simulator.compile_and_run(paths, port, debug) {
                Ok(child) => child,
                Err(e) => {
                    error!("Failed to run {}: {e}", simulator.name());
                    finish();
                    return;
                }
            };

            checked_send(&sender, Message::SimulatorPluginInstanceStarted(child));
            checked_send(&sender, Message::SimulatorPluginConnectTo);
        });
        Some(())
    }

    pub(crate) fn draw_connect_to_simulator_dialog(
        &mut self,
        ctx: &Context,
        msgs: &mut Vec<Message>,
    ) {
        if !self.show_connect_to_simulator_dialog {
            return;
        }

        let mut should_close = false;
        let simulator_port = self.simulator_port.get_or_insert(DEFAULT_SIMULATOR_PORT);

        egui::Window::new("Connect to Running Simulator")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Port");
                ui.add(egui::DragValue::new(simulator_port).range(1..=u16::MAX));

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        msgs.push(Message::SimulatorPluginSetPort(*simulator_port));
                        msgs.push(Message::SimulatorPluginConnectTo);
                        should_close = true;
                    }

                    if ui.button("Cancel").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_connect_to_simulator_dialog = false;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn draw_design_selection_window(&mut self, ctx: &Context, msgs: &mut Vec<Message>) {
        if !self.show_design_selection_window {
            return;
        }

        let mut should_close = false;
        let designs = self.design_selection_items.clone();
        let mut selected_leaf = self.simulator_design.clone();
        let mut bind_address = self
            .simulator_connection_address
            .clone()
            .unwrap_or_default();
        let mut debug = self.simulator_debug;

        egui::Window::new("Select Design")
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match designs {
                    DesignSelectionTree::Libraries(libraries) => {
                        for library in libraries {
                            egui::CollapsingHeader::new(&library.name)
                                .default_open(true)
                                .show(ui, |ui| {
                                    for entity in &library.entities {
                                        egui::CollapsingHeader::new(&entity.name).show(ui, |ui| {
                                            for architecture in &entity.architectures {
                                                let leaf = SimulatorDesign::Architecture {
                                                    library: library.name.clone(),
                                                    entity: entity.name.clone(),
                                                    architecture: architecture.clone(),
                                                };
                                                if ui
                                                    .selectable_label(
                                                        selected_leaf.as_ref() == Some(&leaf),
                                                        architecture,
                                                    )
                                                    .clicked()
                                                {
                                                    selected_leaf = Some(leaf);
                                                }
                                            }
                                        });
                                    }
                                });
                        }
                    }
                    DesignSelectionTree::Modules(modules) => {
                        for module in modules {
                            let leaf = SimulatorDesign::Module {
                                module: module.clone(),
                            };
                            if ui
                                .selectable_label(selected_leaf.as_ref() == Some(&leaf), &module)
                                .clicked()
                            {
                                selected_leaf = Some(leaf);
                            }
                        }
                    }
                    DesignSelectionTree::Empty => {
                        ui.label("No designs found");
                    }
                });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Port");
                    let response =
                        ui.add(egui::TextEdit::singleline(&mut bind_address).desired_width(50.0));
                    if response.changed() && !bind_address.is_empty() {
                        msgs.push(Message::SimulatorPluginSetBindAddress(bind_address.clone()));
                    }
                    ui.checkbox(&mut debug, "Debug");
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(selected_leaf.is_some(), egui::Button::new("OK"))
                        .clicked()
                        && let Some(selected) = selected_leaf.clone()
                    {
                        msgs.push(Message::SimulatorPluginSelectDesign(selected));
                        should_close = true;
                    }

                    if ui.button("Cancel").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_design_selection_window = false;
        }

        self.simulator_design = selected_leaf;
        self.simulator_debug = debug;
    }
}

fn build_hierarchy(
    plugin_hierarchy: &[HierarchyElement],
    time_scale: &TimeScale,
) -> (
    Hierarchy,
    AHashMap<u32, SignalRef>,
    AHashMap<SignalRef, u32>,
) {
    let mut h = HierarchyBuilder::new(time_scale.into(), None, None);
    let mut id_to_signal_ref = AHashMap::new();
    let mut signal_ref_to_id = AHashMap::new();
    for elem in plugin_hierarchy {
        match elem {
            HierarchyElement::PushScope { name, scope_type } => {
                h.push_scope(name, scope_type_to_wellen(*scope_type), None);
            }
            HierarchyElement::PopScope => {
                h.pop_scope();
            }
            HierarchyElement::AddVar {
                id,
                name,
                type_name,
                var_type,
                signal_encoding,
                var_direction,
                enum_map,
                index,
                is_parameter,
            } => {
                let direction = var_direction
                    .map_or_else(wellen::VarDirection::vcd_default, var_direction_to_wellen);

                let signal_ref = h.new_signal(signal_encoding_to_wellen(*signal_encoding));
                id_to_signal_ref.insert(*id, signal_ref);
                signal_ref_to_id.insert(signal_ref, *id);
                let mut enum_id = None;
                if let Some(enum_map) = enum_map {
                    let enum_map: Vec<(&str, &str)> = enum_map
                        .iter()
                        .map(|(option_name, display_value)| {
                            (option_name.as_str(), display_value.as_str())
                        })
                        .collect();
                    enum_id = Some(h.declare_enum(type_name, enum_map.iter().copied()));
                }

                h.add_var(
                    name,
                    var_type_to_wellen(*var_type, *is_parameter),
                    direction,
                    Some(type_name),
                    index.map(var_index_to_wellen),
                    enum_id,
                    signal_ref,
                );
            }
        }
    }

    (h.finish(), id_to_signal_ref, signal_ref_to_id)
}

fn scope_type_to_wellen(scope_type: ScopeType) -> wellen::ScopeType {
    match scope_type {
        ScopeType::Unspecified | ScopeType::Unknown => wellen::ScopeType::Unknown,
        ScopeType::Module => wellen::ScopeType::Module,
        ScopeType::Task => wellen::ScopeType::Task,
        ScopeType::Function => wellen::ScopeType::Function,
        ScopeType::Begin => wellen::ScopeType::Begin,
        ScopeType::Fork => wellen::ScopeType::Fork,
        ScopeType::Generate => wellen::ScopeType::Generate,
        ScopeType::Struct => wellen::ScopeType::Struct,
        ScopeType::Union => wellen::ScopeType::Union,
        ScopeType::Class => wellen::ScopeType::Class,
        ScopeType::Interface => wellen::ScopeType::Interface,
        ScopeType::Package => wellen::ScopeType::Package,
        ScopeType::Program => wellen::ScopeType::Program,
        ScopeType::VhdlArchitecture => wellen::ScopeType::VhdlArchitecture,
        ScopeType::VhdlProcedure => wellen::ScopeType::VhdlProcedure,
        ScopeType::VhdlFunction => wellen::ScopeType::VhdlFunction,
        ScopeType::VhdlRecord => wellen::ScopeType::VhdlRecord,
        ScopeType::VhdlProcess => wellen::ScopeType::VhdlProcess,
        ScopeType::VhdlBlock => wellen::ScopeType::VhdlBlock,
        ScopeType::VhdlForGenerate => wellen::ScopeType::VhdlForGenerate,
        ScopeType::VhdlIfGenerate => wellen::ScopeType::VhdlIfGenerate,
        ScopeType::VhdlGenerate => wellen::ScopeType::VhdlGenerate,
        ScopeType::VhdlPackage => wellen::ScopeType::VhdlPackage,
        ScopeType::GhwGeneric => wellen::ScopeType::GhwGeneric,
        ScopeType::VhdlArray => wellen::ScopeType::VhdlArray,
        ScopeType::Clocking => wellen::ScopeType::Clocking,
        ScopeType::SvArray => wellen::ScopeType::SvArray,
    }
}

fn var_type_to_wellen(var_type: VarType, is_parameter: bool) -> wellen::VarType {
    if is_parameter {
        match var_type {
            VarType::Integer
            | VarType::Int
            | VarType::ShortInt
            | VarType::LongInt
            | VarType::Byte => return wellen::VarType::Parameter,
            VarType::Real | VarType::ShortReal => {
                return wellen::VarType::RealParameter;
            }
            VarType::Event => return wellen::VarType::EventParameter,
            _ => info!(
                "[simulation] Parameter type {var_type:?} is not supported yet; using its regular variable type"
            ),
        }
    }

    match var_type {
        VarType::Unspecified => wellen::VarType::Wire,
        VarType::Event => wellen::VarType::Event,
        VarType::Integer => wellen::VarType::Integer,
        VarType::Real => wellen::VarType::Real,
        VarType::Reg => wellen::VarType::Reg,
        VarType::Time => wellen::VarType::Time,
        VarType::Wire => wellen::VarType::Wire,
        VarType::String => wellen::VarType::String,
        VarType::Port => wellen::VarType::Port,
        VarType::Bit => wellen::VarType::Bit,
        VarType::Logic => wellen::VarType::Logic,
        VarType::Int => wellen::VarType::Int,
        VarType::ShortInt => wellen::VarType::ShortInt,
        VarType::LongInt => wellen::VarType::LongInt,
        VarType::Byte => wellen::VarType::Byte,
        VarType::Enum => wellen::VarType::Enum,
        VarType::ShortReal => wellen::VarType::ShortReal,
        VarType::Boolean => wellen::VarType::Boolean,
        VarType::BitVector => wellen::VarType::BitVector,
        VarType::StdLogic => wellen::VarType::StdLogic,
        VarType::StdLogicVector => wellen::VarType::StdLogicVector,
        VarType::StdULogic => wellen::VarType::StdULogic,
        VarType::StdULogicVector => wellen::VarType::StdULogicVector,
    }
}

fn var_direction_to_wellen(var_direction: VarDirection) -> wellen::VarDirection {
    match var_direction {
        VarDirection::Unspecified | VarDirection::Unknown => wellen::VarDirection::Unknown,
        VarDirection::Implicit => wellen::VarDirection::Implicit,
        VarDirection::Input => wellen::VarDirection::Input,
        VarDirection::Output => wellen::VarDirection::Output,
        VarDirection::InOut => wellen::VarDirection::InOut,
        VarDirection::Buffer => wellen::VarDirection::Buffer,
        VarDirection::Linkage => wellen::VarDirection::Linkage,
    }
}

fn signal_encoding_to_wellen(signal_encoding: SignalEncoding) -> wellen::SignalEncoding {
    match signal_encoding {
        SignalEncoding::String => wellen::SignalEncoding::String,
        SignalEncoding::Real => wellen::SignalEncoding::Real,
        SignalEncoding::BitVector(width) => wellen::SignalEncoding::BitVector(width),
    }
}

fn var_index_to_wellen(index: VarIndex) -> wellen::VarIndex {
    wellen::VarIndex::new(index.msb, index.lsb)
}

fn handle_simulator_response(response: SimulatorToSurferMessage, plugin_tx: &Sender<Message>) {
    match response {
        SimulatorToSurferMessage::SimulatorInfo {
            protocol_version,
            name,
            version,
        } => {
            let simulator_info = SimulatorInfo {
                protocol_version,
                name,
                version,
            };
            info!(
                "[simulation] Connected to simulator '{}' version '{}' (protocol v{})",
                simulator_info.name, simulator_info.version, simulator_info.protocol_version,
            );
            let _ = plugin_tx.send(Message::SimulatorPluginSetSimulatorInfo(Some(
                simulator_info,
            )));
            let _ = plugin_tx.send(Message::SimulatorPluginGetHierarchy);
        }
        SimulatorToSurferMessage::ValueChanges { time_steps, status } => {
            let _ = plugin_tx.send(Message::SimulatorPluginDataReceived(
                time_steps,
                status.into(),
            ));
        }
        SimulatorToSurferMessage::Hierarchy {
            hierarchy,
            exponent,
        } => {
            let _ = plugin_tx.send(Message::SimulatorPluginHierarchyReceived(
                hierarchy,
                protocol_exponent_to_internal(exponent),
            ));
        }
        SimulatorToSurferMessage::CurrentSimulationTime { time } => {
            let _ = plugin_tx.send(Message::SimulatorPluginTimeUpdated(time));
        }
        SimulatorToSurferMessage::Acknowledge { success: _ } => {}
    }
}

fn protocol_exponent_to_internal(exponent: i32) -> TimeScale {
    let unit_exponent = exponent.div_euclid(3) * 3;
    let Some(unit) = TimeUnit::from_exponent(unit_exponent as i8) else {
        return TimeScale::from_fs(1);
    };
    TimeScale {
        unit,
        multiplier: Some(10u32.pow((exponent - unit_exponent) as u32)),
    }
}

/// Function that listens to calls from the plugin, and send calls from surfer to the plugin.
#[cfg(not(target_arch = "wasm32"))]
async fn plugin_connection_loop(
    mut gui_rx: UnboundedReceiver<SurferToSimulatorMessage>,
    plugin_tx: Sender<Message>,
    bind_address: String,
    buffer_size: usize,
) {
    info!("[simulation] connecting to {bind_address}");

    let address = bind_address;
    let max_wait = std::time::Duration::from_secs(20);
    let attempt_timeout = std::time::Duration::from_secs(2);
    let start_time = std::time::Instant::now();
    let mut stream = None;

    while start_time.elapsed() < max_wait {
        match tokio::time::timeout(attempt_timeout, tokio::net::TcpStream::connect(&address)).await
        {
            Ok(Ok(socket)) => {
                stream = Some(socket);
                break;
            }
            Ok(Err(e)) => {
                tracing::error!("[simulation] connect attempt failed: {e}");
            }
            Err(_) => {
                tracing::trace!("[simulation] waiting for plugin on {address}");
            }
        }
    }

    let Some(stream) = stream else {
        error!("[simulation] timed out waiting for {address}");
        let _ = plugin_tx.send(Message::SimulatorPluginRestartFinished);
        return;
    };

    let (read_half, write_half) = stream.into_split();
    let mut writer = FramedWrite::new(
        write_half,
        LengthDelimitedCodec::builder()
            .max_frame_length(buffer_size)
            .new_codec(),
    );
    let mut reader = FramedRead::new(
        read_half,
        LengthDelimitedCodec::builder()
            .max_frame_length(buffer_size)
            .new_codec(),
    );

    info!("[simulation] Connected successfully! Starting listening loop.");
    let _ = plugin_tx.send(Message::SimulatorPluginTcpConnected);

    loop {
        tokio::select! {
            //Checks if Surfer has any messages to send to the plugin
            msg_opt = gui_rx.recv() => {
                if let Some(msg) = msg_opt {
                    let encoded = match encode_surfer_to_simulator_message(&msg) {
                        Ok(encoded) => encoded,
                        Err(e) => {
                            error!("Failed to encode message to simulator: {}", e);
                            continue;
                        }
                    };
                    if let Err(e) = writer.send(Bytes::from(encoded)).await { //Sends the message if nothing goes wrong
                        error!("Failed to send to plugin: {}", e);
                        break;
                    }
                } else {
                    //If the Surfer side has ended the connection
                    info!("[simulation] Channel closed. Shutting down plugin connection.");
                    break;
                }
            }

            frame_opt = reader.next() => { // Checks if a message from the plugin has arrived and sorts it correctly
                match frame_opt {
                    Some(Ok(frame)) => {
                        let response: surfer_simulator_protocol::SimulatorToSurferMessage =
                            match decode_simulator_to_surfer_message(&frame) {
                                Ok(message) => message,
                                Err(e) => {
                                    error!("Failed to decode message from plugin: {}", e);
                                    continue;
                                }
                            };

                        handle_simulator_response(response, &plugin_tx);
                    }
                    Some(Err(e)) => { // If we get an error when trying to read a message
                        error!("[simulation] TCP connection error: {}", e);
                        break;
                    }
                    None => { // If the TCP-Stream has been closed
                        info!("[simulation] TCP stream closed. Shutting down plugin connection.");
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
async fn plugin_connection_loop(
    mut gui_rx: UnboundedReceiver<SurferToSimulatorMessage>,
    plugin_tx: Sender<Message>,
    bind_address: String,
    _buffer_size: usize,
) {
    info!("[simulation] VS Code plugin connection loop started");
    let start_message = serde_json::json!({
        "command": "simulationProxyStart",
        "bindAddress": bind_address,
    });
    notify_host(&start_message.to_string());

    loop {
        tokio::select! {
            msg_opt = gui_rx.recv() => {
                match msg_opt {
                    Some(msg) => {
                        info!("[simulation] Received message from GUI: {:?}", msg);
                        let encoded = match encode_surfer_to_simulator_message(&msg) {
                            Ok(encoded) => encoded,
                            Err(e) => {
                                error!("Failed to encode message to simulator proxy: {}", e);
                                continue;
                            }
                        };
                        let send_message = serde_json::json!({
                            "command": "simulationProxySend",
                            "frame": general_purpose::STANDARD.encode(encoded),
                        });
                        notify_host(&send_message.to_string());
                    }
                    None => {
                        let stop_message = serde_json::json!({ "command": "simulationProxyStop" });
                        notify_host(&stop_message.to_string());
                        break;
                    }
                }
            }

            frame = next_simulation_proxy_message() => {
                let decoded_bytes = match general_purpose::STANDARD.decode(frame) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        error!("Failed to decode simulation proxy frame: {}", e);
                        continue;
                    }
                };
                let response: SimulatorToSurferMessage = match decode_simulator_to_surfer_message(&decoded_bytes) {
                    Ok(message) => message,
                    Err(e) => {
                        error!("Failed to decode message from simulation proxy: {}", e);
                        continue;
                    }
                };

                handle_simulator_response(response, &plugin_tx);
            }
        }
    }
}

impl SystemState {
    pub(crate) fn draw_simulation_starting(&self, ctx: &Context) {
        egui::Window::new("Starting simulator")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Starting simulator...").strong());
                    ui.add_space(6.0);
                    ui.label("Waiting for plugin and hierarchy");
                });
            });
    }

    pub(crate) fn draw_simulation_running(&self, ctx: &Context, msgs: &mut Vec<Message>) {
        egui::Window::new("Simulation is running")
            .anchor(egui::Align2::LEFT_CENTER, egui::vec2(-20.0, -20.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(t) = self.user.plugin_sim_time {
                        let sim_time = if let Some(waves) = self.user.waves.as_ref() {
                            let t_bigint = BigInt::from(t);
                            time_string(
                                &t_bigint,
                                &waves.inner.metadata().timescale,
                                &self.user.wanted_timeunit,
                                &self.get_time_format(),
                            )
                        } else {
                            t.to_string()
                        };

                        ui.label(RichText::new(format!("Simulation time: {sim_time}")).strong());
                    } else {
                        ui.label("Waiting for time from plugin...");
                    }

                    ui.add_space(10.0);

                    if ui.button("Pause simulation").clicked() {
                        msgs.push(Message::SimulatorPluginPause);
                        msgs.push(Message::PauseSimulation);
                    }
                });
            });
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_simulation(&mut self) -> Option<()> {
        use crate::simulation::simulator::Sim;

        let workdir = Utf8PathBuf::from_path_buf(std::env::current_dir().unwrap()).unwrap();
        let simulator = self
            .simulators
            .create_simulator_by_name(self.simulator.as_ref()?, Some(&workdir))?;
        let Sim::TwoStage(simulator) = simulator else {
            error!("Simulator is not a two-stage simulator");
            return None;
        };
        if simulator.is_available() {
            info!(
                "[simulation] {} simulator found, starting simulation",
                simulator.name()
            );
        } else {
            error!(
                "[simulation] {} simulator not found, cannot start simulation",
                simulator.name()
            );
            return None;
        }
        let Some(selected_design) = self.simulator_design.clone() else {
            error!("StartSimulator: failed to get selected simulator design");
            return None;
        };
        self.user.show_command_output = true;
        let sender = self.channels.msg_sender.clone();
        let simulator_plugin_tx = self.channels.simulator_plugin_tx.clone();
        let old_child = self.simulator_child.take();
        if old_child.is_some()
            && let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::ShuttingDown);
        }
        if let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::Starting);
        }
        let port = self.simulator_port;
        let debug = self.simulator_debug;
        crate::async_util::perform_work(move || {
            let finish = || checked_send(&sender, Message::SimulatorPluginRestartFinished);

            // Ask the previous plugin process to shut down before forcing it to exit.
            if let Some(mut old) = old_child {
                if let Some(tx) = &simulator_plugin_tx {
                    let _ = tx.send(SurferToSimulatorMessage::Shutdown);
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while old.try_wait().ok().flatten().is_none()
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if old.try_wait().ok().flatten().is_none() {
                    let _ = old.kill();
                    let _ = old.wait();
                }
            }

            // Spawn simulator plugin process
            let child = match simulator.run(selected_design.clone(), port, debug) {
                Ok(child) => child,
                Err(e) => {
                    error!("Failed to run {}: {e}", simulator.name());
                    finish();
                    return;
                }
            };

            checked_send(&sender, Message::SimulatorPluginInstanceStarted(child));
            checked_send(&sender, Message::SimulatorPluginConnectTo);
        });
        Some(())
    }

    pub(crate) fn connect_to_simulator_plugin(&mut self) {
        // Sets up the TCP connection
        let (tcp_tx, tcp_rx) = unbounded_channel();
        self.channels.simulator_plugin_tx = Some(tcp_tx);

        let plugin_msg_tx = self.channels.simulator_plugin_msg_sender.clone();
        let bind_address = format!(
            "127.0.0.1:{}",
            self.simulator_port.unwrap_or(DEFAULT_SIMULATOR_PORT)
        );
        let bind_address = self
            .simulator_connection_address
            .clone()
            .unwrap_or(bind_address);
        let buffer_size = self.simulator_buffer_size.unwrap_or(MAX_FRAME_SIZE);

        spawn!(plugin_connection_loop(
            tcp_rx,
            plugin_msg_tx,
            bind_address,
            buffer_size,
        ));
    }

    pub(crate) fn simulator_plugin_hierarchy_received(
        &mut self,
        plugin_hierarchy: Vec<HierarchyElement>,
        time_scale: TimeScale,
    ) {
        //This runs when Surfer gets a hierarchy from the plugin. Sends it to wellen correctly
        let (hierarchy, id_to_signal_ref, signal_ref_to_id) =
            build_hierarchy(&plugin_hierarchy, &time_scale);
        self.signal_ref_to_id = Some(signal_ref_to_id);
        let new_waves = Box::new(WaveContainer::new_simulator_waveform(
            hierarchy.into(),
            id_to_signal_ref,
        ));
        #[cfg(not(target_arch = "wasm32"))]
        let sim_design = self
            .simulator_design
            .clone()
            .unwrap_or(SimulatorDesign::ExternalSimulator);
        #[cfg(target_arch = "wasm32")]
        let sim_design = SimulatorDesign::ExternalSimulator;
        self.on_waves_loaded(
            WaveSource::SimulatorPlugin(sim_design.clone()),
            WaveFormat::Simulation,
            new_waves,
            LoadOptions::Clear,
        );

        if let Some(start) = self.simulator_start_time.take() {
            trace!(
                "[simulation] hierarchy loaded: {} ms after starting simulator",
                start.elapsed().as_millis()
            );
        }
        if let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::Paused);
        }
    }

    pub(crate) fn simulator_plugin_data_received(
        &mut self,
        time_steps: Vec<TimeStep>,
        status: SimulationStatus,
    ) {
        if !time_steps.is_empty() {
            let container = self
                .user
                .waves
                .as_mut()
                .unwrap()
                .inner
                .as_waves_mut()
                .unwrap();
            let body_result = container.wellen_add_signals_from_simulation(&time_steps, status);

            match body_result {
                Ok(body) => {
                    #[cfg(not(target_arch = "wasm32"))]
                    let sim_design = self
                        .simulator_design
                        .clone()
                        .unwrap_or(SimulatorDesign::ExternalSimulator);
                    #[cfg(target_arch = "wasm32")]
                    let sim_design = SimulatorDesign::ExternalSimulator;
                    self.update(Message::WaveBodyLoaded(
                        web_time::Instant::now(),
                        WaveSource::SimulatorPlugin(sim_design.clone()),
                        BodyResult::Local(body),
                        Some(status),
                    ));
                }
                Err(err) => error!("Failed to convert plugin values: {:?}", err),
            }
        }
    }

    /// Tell simulator to track specified signals
    pub(crate) fn simulator_track_variables(&mut self, vars: &[VariableRef]) {
        if vars.is_empty() || !self.is_simulation() {
            return;
        }
        let ids = self.variable_refs_to_sim_ids(vars);
        if ids.is_empty() {
            return;
        }
        if let Some(tx) = &self.channels.simulator_plugin_tx {
            let msg = SurferToSimulatorMessage::TrackVariableChanges { ids: ids.clone() };
            if let Err(e) = tx.send(msg) {
                error!("Failed to send track variables message to simulator: {}", e);
            }
        }
    }

    /// Tell simulator to untrack specified signals
    pub(crate) fn simulator_untrack_variables(&mut self, ids: &[SimId]) {
        if ids.is_empty() || !self.is_simulation() {
            return;
        }
        // Removes callbacks from specified signals
        if let Some(tx) = &self.channels.simulator_plugin_tx {
            let msg = SurferToSimulatorMessage::UntrackVariableChanges { ids: ids.to_vec() };
            if let Err(e) = tx.send(msg) {
                error!(
                    "Failed to send untrack variables message to simulator: {}",
                    e
                );
            }
        }
    }

    pub(crate) fn simulator_run_for_duration(&mut self, duration: Option<u64>) {
        if !self.is_simulation() {
            return;
        }
        if let Some(tx) = &self.channels.simulator_plugin_tx {
            self.progress_tracker = Some(LoadProgress::new(LoadProgressStatus::Simulating));
            let msg = SurferToSimulatorMessage::RunSimulation { time: duration };
            if let Err(e) = tx.send(msg) {
                error!("[simulation] Failed to queue RunSimulation to plugin channel: {e}");
            } else if let Some(waves) = self.user.waves.as_mut()
                && let Some(container) = waves.inner.as_waves_mut()
            {
                container.set_simulation_status(SimulationStatus::Running);
            }
        } else {
            error!("Not connected to plugin! (Run ConnectToPlugin first)");
        }
        if let Some(ctx) = &self.context {
            ctx.request_repaint();
        }
    }

    #[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
    fn variable_refs_to_sim_ids(&self, vars: &[VariableRef]) -> Vec<SimId> {
        let container = self
            .user
            .waves
            .as_ref()
            .and_then(|waves| waves.inner.as_waves());
        container
            .map(|container| {
                vars.iter()
                    .filter_map(|var| {
                        if let Ok(SignalId::Wellen(signal_ref)) = container.signal_id(var)
                            && let Some(id) = self
                                .signal_ref_to_id
                                .as_ref()
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
    pub(crate) fn variable_refs_to_sim_ids(&self, vars: &[VariableRef]) -> Vec<SimId> {
        let _ = vars;
        vec![]
    }
}
