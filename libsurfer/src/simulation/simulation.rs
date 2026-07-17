use crate::{
    SystemState,
    channels::checked_send,
    message::Message,
    simulation::{DesignSelectionTree, SimulatorDesign, SimulatorInfo},
    spawn,
    time::time_string,
    wave_container::{SimulationStatus, WaveContainer},
    wave_source::{LoadOptions, WaveFormat, WaveSource},
    wellen::BodyResult,
};
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use base64::{Engine as _, engine::general_purpose};
#[cfg(not(target_arch = "wasm32"))]
use camino::Utf8PathBuf;
use egui::{Context, RichText};
#[cfg(not(target_arch = "wasm32"))]
use futures::SinkExt;
use num::BigInt;
use std::collections::HashMap;
use surfer_simulator_protocol::{
    HierarchyElement, SimulatorToSurferMessage, TimeStep, decode_simulator_to_surfer_message,
    encode_surfer_to_simulator_message,
};
use surfer_time_types::TimeScale;
#[cfg(not(target_arch = "wasm32"))]
use tokio_stream::StreamExt;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::{error, info};
use wellen::{Hierarchy, HierarchyBuilder, SignalRef, VarDirection};

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use crate::wasm_api::{next_simulation_proxy_message, notify_host};

impl SystemState {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn handle_select_files_to_compile(&mut self, simulator: String) {
        self.simulator = Some(simulator);
        self.open_compile_file_dialog();
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
        self.user.show_command_output = true;
        let Some(simulator) = self.simulators.get_simulator_by_name(&simulator) else {
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

    pub(crate) fn draw_connect_to_simulator_dialog(
        &mut self,
        ctx: &Context,
        msgs: &mut Vec<Message>,
    ) {
        if !self.show_connect_to_simulator_dialog {
            return;
        }

        let mut should_close = false;
        let simulator_port = self.simulator_port.get_or_insert(6967);

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
                    DesignSelectionTree::Entities(entities) => {
                        for name in entities {
                            let leaf = SimulatorDesign::Entity {
                                entity: name.clone(),
                            };
                            if ui
                                .selectable_label(selected_leaf.as_ref() == Some(&leaf), &name)
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

    /// Check if we are running a simulation
    pub(crate) fn is_simulation(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .map(|waves| matches!(waves.source, WaveSource::SimulatorPluginDesignSelection(_)))
            .unwrap_or(false)
    }
}

fn build_hierarchy(
    plugin_hierarchy: &[HierarchyElement],
    time_scale: &TimeScale,
) -> (Hierarchy, HashMap<u32, SignalRef>, HashMap<SignalRef, u32>) {
    let mut h = HierarchyBuilder::new(time_scale.into(), None, None);
    let mut id_to_signal_ref = HashMap::new();
    let mut signal_ref_to_id = HashMap::new();
    for elem in plugin_hierarchy {
        match elem {
            HierarchyElement::PushScope { name, scope_type } => {
                h.push_scope(name, *scope_type, None);
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
                scope: _,
            } => {
                let direction = var_direction.unwrap_or_else(VarDirection::vcd_default);

                let signal_ref = h.new_signal(*signal_encoding);
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
                    *var_type,
                    direction,
                    Some(type_name),
                    *index,
                    enum_id,
                    signal_ref,
                );
            }
        }
    }

    (h.finish(), id_to_signal_ref, signal_ref_to_id)
}

fn handle_simulator_response(
    response: SimulatorToSurferMessage,
    plugin_tx: &std::sync::mpsc::Sender<Message>,
) {
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
                "Connected to simulator '{}' version '{}' (protocol v{})",
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
            time_scale,
        } => {
            let _ = plugin_tx.send(Message::SimulatorPluginHierarchyReceived(
                hierarchy, time_scale,
            ));
        }
        SimulatorToSurferMessage::CurrentSimulationTime { time } => {
            let _ = plugin_tx.send(Message::SimulatorTimeUpdated(time));
        }
        SimulatorToSurferMessage::Acknowledge { success: _ } => {}
    }
}

/// Function that listens to calls from the plugin, and send calls from surfer to the plugin.
#[cfg(not(target_arch = "wasm32"))]
async fn plugin_connection_loop(
    mut gui_rx: tokio::sync::mpsc::UnboundedReceiver<
        surfer_simulator_protocol::SurferToSimulatorMessage,
    >,
    plugin_tx: std::sync::mpsc::Sender<Message>,
    bind_address: String,
) {
    info!("[simulation] connecting to {bind_address}");

    let address = bind_address;
    let max_wait = std::time::Duration::from_secs(20);
    let attempt_timeout = std::time::Duration::from_millis(200);
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
    let mut writer = FramedWrite::new(write_half, LengthDelimitedCodec::new());
    let mut reader = FramedRead::new(read_half, LengthDelimitedCodec::new());

    info!("[simulation] Connected successfully! Starting listening loop.");
    let _ = plugin_tx.send(Message::SimulatorPluginTcpConnected);

    loop {
        tokio::select! {
            //Checks if Surfer has any messages to send to the plugin
            msg_opt = gui_rx.recv() => {
                match msg_opt {
                    Some(msg) => {
                        let encoded = match encode_surfer_to_simulator_message(&msg) {
                            Ok(encoded) => encoded,
                            Err(e) => {
                                error!("Failed to encode message to simulator: {}", e);
                                continue;
                            }
                        };
                        if let Err(e) = writer.send(encoded.into()).await { //Sends the message if nothing goes wrong
                            error!("Failed to send to plugin: {}", e);
                            break;
                        }
                    }
                    None => {
                        //If the Surfer side has ended the connection
                        info!("GUI channel closed. Shutting down plugin connection.");
                        break;
                    }
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
                        error!("TCP connection error: {}", e);
                        break;
                    }
                    None => { // If the TCP-Stream has been closed
                        info!("TCP stream closed. Shutting down plugin connection.");
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
async fn plugin_connection_loop(
    mut gui_rx: tokio::sync::mpsc::UnboundedReceiver<
        surfer_simulator_protocol::SurferToSimulatorMessage,
    >,
    plugin_tx: std::sync::mpsc::Sender<Message>,
    bind_address: String,
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
        let workdir = Utf8PathBuf::from_path_buf(std::env::current_dir().unwrap()).unwrap();
        let simulator = self
            .simulators
            .create_simulator_by_name(self.simulator.as_ref()?, Some(workdir.clone()))?;
        if simulator.is_available() {
            info!("{} simulator found, starting simulation", simulator.name());
        } else {
            error!(
                "{} simulator not found, cannot start simulation",
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
        let old_child = self.simulator_child.take();
        self.simulator_starting = true;
        let port = self.simulator_port;
        let debug = self.simulator_debug;
        crate::async_util::perform_work(move || {
            let finish = || checked_send(&sender, Message::SimulatorPluginRestartFinished);

            // Kill the previous plugin process (if any) and wait for it to exit.
            if let Some(mut old) = old_child {
                let _ = old.kill();
                let _ = old.wait();
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
        let (tcp_tx, tcp_rx) = tokio::sync::mpsc::unbounded_channel();
        self.channels.plugin_tx = Some(tcp_tx);

        let plugin_msg_tx = self.channels.plugin_msg_sender.clone();
        let bind_address = format!("127.0.0.1:{}", self.simulator_port.unwrap_or(6967));

        spawn!(plugin_connection_loop(tcp_rx, plugin_msg_tx, bind_address));
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
        self.on_waves_loaded(
            WaveSource::SimulatorPluginDesignSelection(
                self.simulator_design
                    .clone()
                    .unwrap_or(SimulatorDesign::ExternalSimulator),
            ),
            WaveFormat::Simulation,
            new_waves,
            LoadOptions::Clear,
        );

        if let Some(start) = self.simulator_start_time.take() {
            info!(
                "[TIMING] hierarchy loaded: {} ms after starting simulator",
                start.elapsed().as_millis()
            );
        }
        if let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::Paused);
        }
        self.simulator_starting = false;
    }

    pub(crate) fn simulator_plugin_data_received(
        &mut self,
        time_steps: Vec<TimeStep>,
        status: SimulationStatus,
    ) {
        if !time_steps.is_empty() {
            // Adds the data correctly in wellen so it can be shown
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
                    self.update(Message::WaveBodyLoaded(
                        web_time::Instant::now(),
                        WaveSource::SimulatorPluginDesignSelection(
                            self.simulator_design
                                .clone()
                                .unwrap_or(SimulatorDesign::ExternalSimulator),
                        ),
                        BodyResult::Local(body),
                        Some(status),
                    ));
                }
                Err(err) => error!("Failed to convert plugin values: {:?}", err),
            }
        }
    }
}
