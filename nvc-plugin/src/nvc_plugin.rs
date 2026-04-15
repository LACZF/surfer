use futures::{SinkExt, StreamExt};
use surfer_simulator_protocol::{
    HierarchyElem, PluginToSurferMessage, SignalKind, SurferToPluginMessage, VarDirection,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use std::sync::{Arc, OnceLock};
use std::sync::Mutex;
use tokio::sync::Notify;
use vhpi::Handle;
use surfer_simulator_protocol::SignalChange;
use surfer_simulator_protocol::TimeStep;

static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static START_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static HIERARCHY: OnceLock<Region> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<Vec<TimeStep>>> =
    OnceLock::new();


fn value_change(data: &vhpi::CbData) {
    let val = data.obj.get_value(vhpi::Format::ObjType);
    match val {
        Ok(value) => {
            let full_name = data.obj.get_str(vhpi::StrProperty::FullName).unwrap();
            vhpi::printf!("value change {} => {}", full_name, value);
            VALUE_CHANGES.get().unwrap().lock().unwrap().push(
                surfer_simulator_protocol::ValueChange {
                    timestamp: vhpi::get_time().into(),
                    signal_name: full_name,
                    value: value.into(),
                },
            );
        }
        Err(err) => {
            let full_name = data.obj.get_str(vhpi::StrProperty::FullName).unwrap();
            let kind = data.obj.get_kind();
            let type_handle = data.obj.handle(vhpi::OneToOne::Type);
            match type_handle.get_kind() {
                Some(vhpi::ClassKind::ArrayTypeDecl) => {
                    let elem_type_handle = type_handle.handle(vhpi::OneToOne::ElemType);
                    for (index, i) in type_handle.index_range().enumerate() {
                        if let Some(indexed_handle) = data
                            .obj
                            .handle_by_index(vhpi::OneToMany::IndexedNames, index as i32)
                        {
                            vhpi::printf!(
                                "element {} of array {} is of type {} with value {}",
                                i,
                                data.obj.get_name().unwrap(),
                                elem_type_handle.get_name().unwrap(),
                                indexed_handle
                                    .get_value(vhpi::Format::ObjType)
                                    .unwrap_or(vhpi::Value::Unknown)
                            );
                        }
                    }
                }
                Some(vhpi::ClassKind::RecordTypeDecl) => {
                    for field_handle in data.obj.iterator(vhpi::OneToMany::SelectedNames) {
                        let field_type_handle = field_handle.handle(vhpi::OneToOne::Type);

                        vhpi::printf!(
                            "signal {} is a field of the record {} of type {} with value {}",
                            field_handle.get_name().unwrap(),
                            data.obj.get_name().unwrap(),
                            field_type_handle.get_name().unwrap(),
                            field_handle
                                .get_value(vhpi::Format::ObjType)
                                .unwrap_or(vhpi::Value::Unknown)
                        );
                    }
                }
                Some(type_kind) => {
                    vhpi::printf!(
                        "value change on {}, but failed to get value: {} (kind: {:?}, type: {}, type kind: {:?})",
                        full_name,
                        err,
                        kind,
                        type_handle.get_name().unwrap(),
                        type_kind
                    );
                }
                None => {
                    vhpi::printf!("Invalid kind for {}: {}", full_name, err);
                }
            }
        }
    }
}

// Get the hierarchy so that it can be sent back to Surfer
// TODO: What we need to tore will probably change so that we have the information we need in Surfer. We'll probably need some sort of ID or handle of some kind for each signal so that Surfer and the plugin knows that they are talking about the same signal
pub fn collect_region(region: &vhpi::Handle, hierarchy: &mut Vec<HierarchyElem>) {
    hierarchy.push(HierarchyElem::PushScope {
        name: region.get_name().unwrap_or_default(),
    });

    for port in region.iterator(vhpi::OneToMany::PortDecls) {
        hierarchy.push(HierarchyElem::AddVar {
            name: port.get_name().unwrap_or_default(),
            kind: SignalKind::Scalar {
                type_name: "port".into(),
            },
            direction: VarDirection::Input,
        });
    }

    for sig in region.iterator(vhpi::OneToMany::SigDecls) {
        let type_handle = sig.handle(vhpi::OneToOne::Type);
        let kind = match type_handle.get_kind() {
            Some(vhpi::ClassKind::ArrayTypeDecl) => {
                let elements = type_handle
                    .index_range()
                    .enumerate()
                    .filter_map(|(index, _)| {
                        sig.handle_by_index(vhpi::OneToMany::IndexedNames, index as i32)
                            .and_then(|h| h.get_name())
                    })
                    .collect();
                SignalKind::Array {
                    type_name: type_handle.get_name().unwrap_or_default(),
                    elements,
                }
            }
            Some(vhpi::ClassKind::RecordTypeDecl) => {
                let fields = sig
                    .iterator(vhpi::OneToMany::SelectedNames)
                    .filter_map(|f| f.get_name())
                    .collect();
                SignalKind::Record {
                    type_name: type_handle.get_name().unwrap_or_default(),
                    fields,
                }
            }
            Some(vhpi::ClassKind::EnumTypeDecl) => SignalKind::Enum {
                type_name: type_handle.get_name().unwrap_or_default(),
                literals: type_handle.enum_literals().unwrap_or_default(),
            },
            Some(_) => SignalKind::Scalar {
                type_name: type_handle.get_name().unwrap_or_default(),
            },
            None => SignalKind::Unknown,
        };

        hierarchy.push(HierarchyElem::AddVar {
            name: sig.get_name().unwrap_or_default(),
            kind,
            direction: VarDirection::Output,
        });
    }

    for sub in region.iterator(vhpi::OneToMany::InternalRegions) {
        collect_region(&sub, hierarchy); // recurse
    }

    hierarchy.push(HierarchyElem::PopScope);
}

fn walk_region(region: &vhpi::Handle) {
    for port in region.iterator(vhpi::OneToMany::PortDecls) {
        println!("port {}", port.get_name().unwrap());
        let _ = port.register_cb(vhpi::CbReason::ValueChange, value_change);
    }

    for sig in region.iterator(vhpi::OneToMany::SigDecls) {
        let type_handle = sig.handle(vhpi::OneToOne::Type);
        match type_handle.get_kind() {
            Some(vhpi::ClassKind::ArrayTypeDecl) => {
                match sig.get_value(vhpi::Format::ObjType) {
                    Ok(_) => {
                        println!(
                            "signal {} is an array of type {}",
                            sig.get_name().unwrap(),
                            type_handle.get_name().unwrap()
                        );
                    }
                    Err(err) => {
                        let elem_type_handle = type_handle.handle(vhpi::OneToOne::ElemType);
                        for (index, i) in type_handle.index_range().enumerate() {
                            println!(
                                "element {i} of array {} is of type {}",
                                sig.get_name().unwrap(),
                                elem_type_handle.get_name().unwrap(),
                            );
                            if let Some(h) =
                                sig.handle_by_index(vhpi::OneToMany::IndexedNames, index as i32)
                            {
                                if let Err(e) =
                                    h.register_cb(vhpi::CbReason::ValueChange, value_change)
                                {
                                    vhpi::printf!(
                                        "failed to register callback for element {i}: {:?}",
                                        e
                                    );
                                }
                            }
                        }
                        println!(
                            "signal {} is an array of type {}",
                            sig.get_name().unwrap(),
                            type_handle.get_name().unwrap()
                        );
                        println!("but failed to get value: {err}");
                    }
                }
                let _ = sig.register_cb(vhpi::CbReason::ValueChange, value_change);
            }
            Some(vhpi::ClassKind::RecordTypeDecl) => {
                println!(
                    "signal {} is a record of type {}",
                    sig.get_name().unwrap(),
                    type_handle.get_name().unwrap()
                );
                for field in sig.iterator(vhpi::OneToMany::SelectedNames) {
                    let field_type_handle = field.handle(vhpi::OneToOne::Type);
                    println!(
                        "field {} of record {} (type: {})",
                        field.get_name().unwrap(),
                        sig.get_name().unwrap(),
                        field_type_handle.get_name().unwrap()
                    );
                    if let Err(e) = field.register_cb(vhpi::CbReason::ValueChange, value_change) {
                        vhpi::printf!(
                            "failed to register callback for field {}: {:?}",
                            field.get_name().unwrap(),
                            e
                        );
                    }
                }
                let _ = sig.register_cb(vhpi::CbReason::ValueChange, value_change);
            }
            Some(vhpi::ClassKind::EnumTypeDecl) => {
                println!(
                    "signal {} is an enum of type {} with values {:?}",
                    sig.get_name().unwrap(),
                    type_handle.get_name().unwrap(),
                    type_handle.enum_literals().unwrap_or_default()
                );
                let _ = sig.register_cb(vhpi::CbReason::ValueChange, value_change);
            }
            Some(kind) => {
                println!(
                    "signal {}, type {} (kind: {:?})",
                    sig.get_name().unwrap(),
                    type_handle.get_name().unwrap(),
                    kind
                );
                let _ = sig.register_cb(vhpi::CbReason::ValueChange, value_change);
            }
            None => {
                println!("signal {} with unsupported kind", sig.get_name().unwrap(),);
            }
        }
    }

    for sub in region.iterator(vhpi::OneToMany::InternalRegions) {
        println!("internal region {}", sub.get_name().unwrap());
        walk_region(&sub);
    }
}

fn start_of_sim(_data: &vhpi::CbData) {
    vhpi::printf("start of simulation");

    let root = vhpi::handle(vhpi::OneToOne::RootInst);

    vhpi::printf!("root name is {}", root.get_name().unwrap());
    vhpi::printf!("root kind is {:?}", root.get_kind());

    // TODO: We should read the hierarchy here, so that it can be sent back
    walk_region(&root);
    let mut hierarchy = Vec::new();
    collect_region(&root, &mut hierarchy);
    HIERARCHY.set(Mutex::new(hierarchy)).ok(); // store it

    // Block here until message is received
    vhpi::printf("waiting for StartSimulation message...");
    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    vhpi::printf("simulation started!");
}

fn next_time_step(_data: &vhpi::CbData) {
    let (time, status) = vhpi::get_next_time();
    vhpi::printf!("next time step: {} (status: {status})", time);
}

fn end_of_sim(_data: &vhpi::CbData) {
    let time = vhpi::get_time();
    let cycles = vhpi::get_cycles();
    let time_resolution = vhpi::simulator_time_resolution();
    let total_time = time * time_resolution;
    vhpi::printf!(
        "end of simulation at time {total_time} ({cycles} cycle{})",
        if cycles == 1 { "" } else { "s" }
    );

    dbg!(VALUE_CHANGES.get().unwrap().lock().unwrap().clone());
}

async fn handle_connection(stream: TcpStream) {
    let (read_half, write_half) = stream.into_split();

    let mut reader = FramedRead::new(read_half, LengthDelimitedCodec::new());
    let mut writer = FramedWrite::new(write_half, LengthDelimitedCodec::new());

    if let Some(frame) = reader.next().await {
        let msg: SurferToPluginMessage = bincode::deserialize(&frame.unwrap()).unwrap();
        let response: PluginToSurferMessage = match msg {
            SurferToPluginMessage::GetSimulatorName => {
                let name = vhpi::simulator_name().unwrap();
                PluginToSurferMessage::SimulatorName(name)
            }
            SurferToPluginMessage::StartSimulation => {
                START_NOTIFY.get().unwrap().notify_one();
                PluginToSurferMessage::Acknowledge(true, SurferToPluginMessage::StartSimulation)
            }
            SurferToPluginMessage::GetHierarchy => {
                let hierarchy = HIERARCHY.get().unwrap().lock().unwrap().clone();

                PluginToSurferMessage::Hierarchy(hierarchy)
            }
            SurferToPluginMessage::SendValues => {
                //Locks and takes the values from VALUE_CHANGES and sends them to Surfer
                let mut global_vec = VALUE_CHANGES.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap();
                let value_changes = std::mem::take(&mut *global_vec);
                PluginToSurferMessage::SendChangedValues(value_changes)
                //The mutex lock is released here automaticly
            }
            SurferToPluginMessage::SetCallback(id) => {
                set_callback_on_signal(id);
                PluginToSurferMessage::Acknowledge(true, msg)
            }
            _ => {
                dbg!(&msg);
                PluginToSurferMessage::Acknowledge(false, msg)
            }       
        };

        // Send a response to the message
        let encoded = bincode::serialize(&response).unwrap();
        writer.send(encoded.into()).await.unwrap();
    }
}

async fn message_loop() {
    let listener = TcpListener::bind("0.0.0.0:6967").await.unwrap();

    loop {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::spawn(handle_connection(stream));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nvc_plugin_startup() {
    // TODO: Program exits when simulation is done. We should control it from the message loop part? (Or at least notify Surfer that the process exited)

    VALUE_CHANGES.set(Mutex::new(Vec::new())).ok();

    START_NOTIFY.set(Arc::new(Notify::new())).ok();

    // Set up communication over TCP
    let runtime = tokio::runtime::Runtime::new().unwrap();
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();
    runtime.spawn(message_loop());

    Box::leak(Box::new(runtime)); // keep runtime alive

    // TODO: Wait until simulation has been started from surfer
    // Maybe we'll need to block in StartOfSimulation instead since we want to be able to read the hierarchy first

    vhpi::printf("dumper plugin loaded");
    vhpi::printf!("simulator name: {}", vhpi::simulator_name().unwrap());
    vhpi::printf!("simulator version: {}", vhpi::simulator_version().unwrap());
    vhpi::printf!(
        "simulator time resolution: {}",
        vhpi::simulator_time_resolution()
    );
    vhpi::printf!(
        "simulator capabilities: {:?}",
        vhpi::simulator_capabilities()
    );
    vhpi::printf!("simulator random seed: {:?}", vhpi::simulator_random_seed());

    vhpi::assert!(
        false,
        vhpi::Severity::Warning,
        "This is a test assertion with severity Warning"
    );

    let _ = vhpi::register_cb(vhpi::CbReason::StartOfSimulation, start_of_sim);
    let _ = vhpi::register_cb(vhpi::CbReason::EndOfSimulation, end_of_sim);
    let _ = vhpi::register_cb(vhpi::CbReason::NextTimeStep, next_time_step);
}


fn set_callback_on_signal(signal: u8){
    //Function that finds the input signals and puts a value changed callback on it
    println!("SignalsID: {}", signal)

    //Find the signal from a hashmap, list or something else
    //When we have the name we do vhpiHandleT vhpi_handle_by_name(const VhpiStringT name, vhpiHandleT scope);

    //Then we just set the callback on it
    //signal.register_cb(vhpi::CbReason::ValueChanged, ValueFunction(self))

}

fn value_function(signal_handle: Handle){
    let mut global_vec = VALUE_CHANGES.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap();

    //Finds the value of the signal when ValueChanged has been called and then creates a SignalChanged for that change

    //TODO: Do this instead of what is now used let sig_value = signal_handle.get_value(Format::ObjType);
    let sig_value:u8 = 5;
    let current_time:u64 = 9;
    let sig_id = 5; //TODO: We can only get the signal name from the handle so we need the signal table to look up the id from the name
    let change:SignalChange = SignalChange{signal_id:sig_id, value:sig_value};
    
    //Find the TimeStep if it exists, if it doesn't exist then we need to create it
    if let Some(time_step) = global_vec.iter_mut().find(|ts| ts.time_fs == current_time) {
        time_step.changes.push(change) ;
    } else{
        let time_step:TimeStep = TimeStep{time_fs:current_time, changes:[change].to_vec()};
        global_vec.push(time_step);
    }
    //TimeStep.changes.put(change)
    //VALUE_CHANGES.get().unwrap().lock().unwrap().push(TimeStep)
}

//TODO: Value changed och update_time funktionerna måste skapa TimeSteps och SignalValues
//TODO: Skicka alla värde ändingar kontinuerligt
//TODO: Hash_Map eller liknande behövs för att mappa signal id mellan surfer och plugin
//TODO: Implementera starta, stoppa, pausa och tidsstega simulering
//TODO: Omvandla signal värdet från Value struct till "riktig" variabel typ (t.ex string eller u8)