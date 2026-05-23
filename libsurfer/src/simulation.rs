use crate::{
    SystemState,
    channels::checked_send,
    message::Message,
    wave_container::WaveContainer,
    wave_source::{LoadOptions, WaveFormat, WaveSource},
};
use futures::SinkExt;
use surfer_simulator_protocol::{HierarchyElem, PluginToSurferMessage, TimeUnit};
use tokio::net::TcpStream;
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::{error, info};

fn convert_time_unit(time_unit: TimeUnit) -> ::wellen::Timescale {
    let unit = match time_unit {
        TimeUnit::ZeptoSeconds => ::wellen::TimescaleUnit::ZeptoSeconds,
        TimeUnit::AttoSeconds => ::wellen::TimescaleUnit::AttoSeconds,
        TimeUnit::FemtoSeconds => ::wellen::TimescaleUnit::FemtoSeconds,
        TimeUnit::PicoSeconds => ::wellen::TimescaleUnit::PicoSeconds,
        TimeUnit::NanoSeconds => ::wellen::TimescaleUnit::NanoSeconds,
        TimeUnit::MicroSeconds => ::wellen::TimescaleUnit::MicroSeconds,
        TimeUnit::MilliSeconds => ::wellen::TimescaleUnit::MilliSeconds,
        TimeUnit::Seconds => ::wellen::TimescaleUnit::Seconds,
    };

    ::wellen::Timescale::new(1, unit)
}

pub fn build_hierarchy(
    plugin_hierarchy: &[HierarchyElem],
    time_unit: surfer_simulator_protocol::TimeUnit,
) -> ::wellen::Hierarchy {
    let mut h = ::wellen::HierarchyBuilder::new(::wellen::FileFormat::Memory);

    h.set_timescale(convert_time_unit(time_unit));

    for elem in plugin_hierarchy {
        match elem {
            HierarchyElem::PushScope { name, scope_type } => {
                let name = h.add_string(name.clone().into());
                let scope_type = *scope_type;
                h.add_scope(name, None, scope_type, None, None, None, false);
            }
            HierarchyElem::PopScope => {
                h.pop_scope();
            }
            HierarchyElem::AddVar {
                id,
                name,
                length,
                type_name,
                var_type,
                signal_encoding,
            } => {
                let direction = ::wellen::VarDirection::vcd_default(); // TODO
                let enum_type = None; // TODO

                let type_name = Some(h.add_string(type_name.clone().into()));

                let (var_name, index, scopes) =
                    ::wellen::parse_name(name.as_bytes(), *length).unwrap();

                let name = h.add_string(var_name);
                let signal_ref = ::wellen::SignalRef::from_index(*id as usize).unwrap();

                let num_scopes = scopes.len();

                h.add_array_scopes(scopes);

                h.add_var(
                    name,
                    *var_type,
                    *signal_encoding,
                    direction,
                    index,
                    signal_ref,
                    enum_type,
                    type_name,
                );

                h.pop_scopes(num_scopes);
            }
        }
    }

    h.finish()
}

/// Function that listens to calls from the plugin, and send calls from surfer to the plugin
pub(crate) async fn plugin_connection_loop(
    mut gui_rx: tokio::sync::mpsc::UnboundedReceiver<
        surfer_simulator_protocol::SurferToPluginMessage,
    >,
    plugin_tx: std::sync::mpsc::Sender<Message>,
) {
    info!("[plugin] connecting to 127.0.0.1:6967");

    let address = "127.0.0.1:6967";
    let max_wait = std::time::Duration::from_secs(5);
    let attempt_timeout = std::time::Duration::from_millis(200);
    let start_time = std::time::Instant::now();
    let mut stream = None;

    while start_time.elapsed() < max_wait {
        match tokio::time::timeout(attempt_timeout, TcpStream::connect(address)).await {
            Ok(Ok(socket)) => {
                stream = Some(socket);
                break;
            }
            Ok(Err(e)) => {
                tracing::trace!("[plugin] connect attempt failed: {e}");
            }
            Err(_) => {
                tracing::trace!("[plugin] waiting for plugin on {address}");
            }
        }
    }

    let Some(stream) = stream else {
        error!("[plugin] timed out waiting for {address}");
        let _ = plugin_tx.send(Message::NvcRestartFinished);
        return;
    };

    let (read_half, write_half) = stream.into_split();
    let mut writer = FramedWrite::new(write_half, LengthDelimitedCodec::new());
    let mut reader = FramedRead::new(read_half, LengthDelimitedCodec::new());

    info!("[plugin] Connected successfully! Starting listening loop.");
    let _ = plugin_tx.send(Message::NvcPluginTcpConnected);
    //When we know the connection is set up we ask for the hierarchy
    let _ = plugin_tx.send(Message::GetPluginHierarchy);

    loop {
        tokio::select! {
            //Checks if Surfer has any messages to send to the plugin
            msg_opt = gui_rx.recv() => {
                match msg_opt {
                    Some(msg) => {
                        let encoded = bincode::serialize(&msg).unwrap();
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
                        let response: surfer_simulator_protocol::PluginToSurferMessage = bincode::deserialize(&frame).unwrap();

                        match response {
                            PluginToSurferMessage::SendChangedValues(time_steps, is_finished) => {
                                let _ = plugin_tx.send(Message::PluginDataReceived(
                                    time_steps,
                                    is_finished,
                                ));
                            }
                            PluginToSurferMessage::Hierarchy(hierarchy, time_unit) => {
                                let _ = plugin_tx.send(Message::PluginHierarchyReceived(
                                    hierarchy,
                                    time_unit,
                                ));
                            }
                            PluginToSurferMessage::SendUpdatedTime(time) => {
                                let _ = plugin_tx.send(Message::TimeUpdated(time));
                            }
                            PluginToSurferMessage::Acknowledge(_) => {
                                info!("Received ACK from plugin");
                            }
                            _ => {
                                info!("Received other message from plugin: {:?}", response);
                            }
                        }
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

impl SystemState {
    pub(crate) fn start_vhdl_simulation(&mut self) -> Option<()> {
        let vhdl_path = self.vhdl_file_path.clone()?;
        let sender = self.channels.msg_sender.clone();
        let old_child = self.nvc_child.take();
        self.nvc_starting = true;
        crate::async_util::perform_work(move || {
            let finish = || checked_send(&sender, Message::NvcRestartFinished);

            // Kill the previous plugin process (if any) and wait for it to exit.
            if let Some(mut old) = old_child {
                let _ = old.kill();
                let _ = old.wait();
            }

            let Some(tb_name) = vhdl_path.file_stem().map(str::to_string) else {
                error!("StartNvcSimulation: failed to get testbench name from path");
                finish();
                return;
            };

            // Always run plugin from /nvc-plugin folder
            let nvc_workdir = std::env::current_dir().unwrap().join("nvc-plugin");
            let filename = format!("libnvc_plugin.{}", std::env::consts::DLL_EXTENSION);
            let plugin_path = nvc_workdir.join("../target/debug").join(filename);

            // Run nvc commands after each other (-a, -e, -r)
            match std::process::Command::new("nvc")
                .current_dir(&nvc_workdir)
                .args(["-a", vhdl_path.as_str()])
                .status()
            {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    error!("nvc -a exited with status {s}");
                    finish();
                    return;
                }
                Err(e) => {
                    error!("Failed to run nvc -a: {e}");
                    finish();
                    return;
                }
            }

            match std::process::Command::new("nvc")
                .current_dir(&nvc_workdir)
                .args(["-e", &tb_name])
                .status()
            {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    error!("nvc -e exited with status {s}");
                    finish();
                    return;
                }
                Err(e) => {
                    error!("Failed to run nvc -e: {e}");
                    finish();
                    return;
                }
            }

            // Spawn nvc -r --load=... (this is the long-running plugin process)
            let child = match std::process::Command::new("nvc")
                .current_dir(&nvc_workdir)
                .args(["-r", &tb_name])
                .arg(format!("--load={}", plugin_path.display()))
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    error!("Failed to run nvc -r: {e}");
                    finish();
                    return;
                }
            };

            checked_send(&sender, Message::NvcChildStarted(child));
            checked_send(&sender, Message::ConnectToPlugin);
        });
        Some(())
    }

    pub(crate) fn connect_to_plugin(&mut self) {
        // Sets up the TCP connection
        let (tcp_tx, tcp_rx) = tokio::sync::mpsc::unbounded_channel();
        self.channels.plugin_tx = Some(tcp_tx);

        let plugin_msg_tx = self.channels.plugin_msg_sender.clone();

        // Create a background thread to handle plugin messages
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(plugin_connection_loop(tcp_rx, plugin_msg_tx));
        });
    }

    pub(crate) fn plugin_hierarchy_received(
        &mut self,
        plugin_hierarchy: Vec<surfer_simulator_protocol::HierarchyElem>,
        time_unit: surfer_simulator_protocol::TimeUnit,
    ) {
        //This runs when Surfer gets a hierarchy from the plugin. Sends it to wellen correctly
        let hierarchy = build_hierarchy(&plugin_hierarchy, time_unit);
        let new_waves = Box::new(WaveContainer::new_waveform(hierarchy.into()));
        self.on_waves_loaded(
            WaveSource::Data,
            WaveFormat::Simulation,
            new_waves,
            LoadOptions::Clear,
        );

        if let Some(start) = self.nvc_start_time.take() {
            info!(
                "[TIMING] hierarchy loaded: {} ms after nvc -r",
                start.elapsed().as_millis()
            );
        }
        self.nvc_starting = false;
    }
}
