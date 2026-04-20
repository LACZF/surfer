use futures::{SinkExt, StreamExt};
use surfer_simulator_protocol::{
    HierarchyElem, PluginToSurferMessage, SignalID, SignalKind, SurferToPluginMessage, TimeUnit,
    VarDirection,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use dashmap::DashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use surfer_simulator_protocol::SignalChange;
use surfer_simulator_protocol::TimeStep;
use tokio::sync::Notify;
use vhpi::Handle;

// TODO: Split into multiple files, e.g.
// - hierarchy.rs
// - id_store.rs
// - value_change.rs
// - callbacks.rs ??

static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static START_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static HIERARCHY: OnceLock<Mutex<Vec<HierarchyElem>>> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<Vec<TimeStep>>> = OnceLock::new();

static ID_STORE: OnceLock<Arc<IdStore>> = OnceLock::new();
// The protocol will use ID's to handle names so we need some way to keep track of them
pub struct IdStore {
    next_id: AtomicU64,
    str_to_id: DashMap<String, u64>,
    id_to_str: DashMap<u64, String>,
}

impl IdStore {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            str_to_id: DashMap::new(),
            id_to_str: DashMap::new(),
        }
    }

    /// Get existing ID or create a new one
    pub fn get_or_create_id(&self, s: &str) -> u64 {
        if let Some(id) = self.str_to_id.get(s) {
            return *id;
        }

        // slow path: create new ID
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        // double-check race condition
        match self.str_to_id.entry(s.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(e) => *e.get(),
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(id);
                self.id_to_str.insert(id, s.to_string());
                id
            }
        }
    }

    pub fn get_str(&self, id: u64) -> Option<String> {
        self.id_to_str.get(&id).map(|v| v.clone())
    }

    pub fn get_id(&self, s: &str) -> Option<u64> {
        self.str_to_id.get(s).map(|v| *v)
    }
}

fn value_change(data: &vhpi::CbData) {
    let val = data.obj.get_value(vhpi::Format::ObjType);
    match val {
        Ok(value) => {
            let full_name = data.obj.get_str(vhpi::StrProperty::FullName).unwrap();
            vhpi::printf!("value change {} => {}", full_name, value);

            value_function(&data.obj);
        }
        Err(err) => {
            // TODO: I can't be bothered to read this right now, make sure everything looks fine

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
    let id_store = ID_STORE.get().unwrap();

    hierarchy.push(HierarchyElem::PushScope {
        name: region.get_name().unwrap_or_default(),
    });

    for port in region.iterator(vhpi::OneToMany::PortDecls) {
        hierarchy.push(HierarchyElem::AddVar {
            name: port.get_name().unwrap_or_default(),
            id: id_store
                .get_or_create_id(&port.get_full_name().unwrap_or_default())
                .into(),
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

        // TODO: I don't think we handle direction correctly
        hierarchy.push(HierarchyElem::AddVar {
            name: sig.get_name().unwrap_or_default(),
            id: id_store
                .get_or_create_id(&sig.get_full_name().unwrap_or_default())
                .into(),
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

    thread::park(); // blocks indefinitely
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

                // VHPI always uses femtoseconds
                PluginToSurferMessage::Hierarchy(hierarchy, TimeUnit::FemtoSeconds)
            }
            SurferToPluginMessage::SendValues => {
                //Locks and takes the values from VALUE_CHANGES and sends them to Surfer
                let mut global_vec = VALUE_CHANGES
                    .get_or_init(|| Mutex::new(Vec::new()))
                    .lock()
                    .unwrap();
                let value_changes = std::mem::take(&mut *global_vec);
                PluginToSurferMessage::SendChangedValues(value_changes)
                //The mutex lock is released here automatically
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
    ID_STORE.set(Arc::new(IdStore::new())).ok();

    // Set up communication over TCP
    let runtime = tokio::runtime::Runtime::new().unwrap();
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();
    runtime.spawn(message_loop());

    Box::leak(Box::new(runtime)); // TODO: This is really ugly, but it keeps the runtime alive

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

fn set_callback_on_signal(signal: SignalID) {
    //Function that finds the input signals and puts a value changed callback on it
    println!("SignalsID: {}", signal.0)

    //Find the signal from a hashmap, list or something else
    //When we have the name we do vhpiHandleT vhpi_handle_by_name(const VhpiStringT name, vhpiHandleT scope);

    //Then we just set the callback on it
    //signal.register_cb(vhpi::CbReason::ValueChanged, ValueFunction(self))

    // TODO: Record the current value to VALUE_CHANGES, otherwise we don't get the initial value!
}

// Finds the value of the signal when ValueChanged has been called and then creates a SignalChanged for that change
#[allow(unused)]
fn value_function(signal_handle: &Handle) {
    let mut global_changes = VALUE_CHANGES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();

    let vhpi_value = signal_handle.get_value(vhpi::Format::ObjType).unwrap();
    let sig_value = vhpi_value_to_vcd_bytes(&vhpi_value).unwrap();

    let current_time = vhpi::get_time().to_i64();

    let id_store = ID_STORE.get().unwrap();
    let id = id_store
        .get_id(&signal_handle.get_full_name().unwrap_or_default())
        .unwrap();

    let change: SignalChange = SignalChange {
        id: id.into(),
        value: sig_value,
    };

    // Find the TimeStep if it exists, if it doesn't exist then we need to create it
    if let Some(time_step) = global_changes
        .iter_mut()
        .find(|ts| ts.timestamp == current_time)
    {
        time_step.changes.push(change);
    } else {
        let time_step: TimeStep = TimeStep {
            timestamp: current_time,
            changes: [change].to_vec(),
        };
        global_changes.push(time_step);
    }
}

//TODO: ValueChanged and UpdateTime functions must create TimeSteps and SignalValues
//TODO: Send all value changes continuously
//TODO: A HashMap or similar is needed to map signal IDs between Surfer and the plugin
//TODO: Implement start, stop, pause, and time-step simulation
//TODO: Convert the signal value from the Value struct to a "real" variable type (e.g., string or u8)

fn logic_val_to_char(val: &vhpi::LogicVal) -> u8 {
    match val {
        vhpi::LogicVal::Zero => b'0',
        vhpi::LogicVal::One => b'1',
        vhpi::LogicVal::X => b'x',
        vhpi::LogicVal::Z => b'z',
        vhpi::LogicVal::H => b'h',
        vhpi::LogicVal::U => b'u',
        vhpi::LogicVal::W => b'w',
        vhpi::LogicVal::L => b'l',
        vhpi::LogicVal::DontCare => b'-',
        vhpi::LogicVal::Unknown(v) => *v,
    }
}

fn int_to_bitvec_chars(value: u64, bits: usize) -> Vec<u8> {
    (0..bits)
        .rev()
        .map(|i| if (value >> i) & 1 == 1 { b'1' } else { b'0' })
        .collect()
}

fn vhpi_value_to_vcd_bytes(value: &vhpi::Value) -> Option<Vec<u8>> {
    match value {
        // Already binary string — just add 'b' prefix
        vhpi::Value::BinStr(s) => {
            let mut buf = Vec::with_capacity(s.len() + 1);
            buf.push(b'b');
            buf.extend_from_slice(s.as_bytes());
            Some(buf)
        }

        // Expand each octal digit to 3 binary chars
        vhpi::Value::OctStr(s) => {
            let mut buf = vec![b'b'];
            for ch in s.chars() {
                let digit = ch.to_digit(8)? as u8;
                buf.push(if digit & 4 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 2 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 1 != 0 { b'1' } else { b'0' });
            }
            Some(buf)
        }

        // Expand each hex digit to 4 binary chars
        vhpi::Value::HexStr(s) => {
            let mut buf = vec![b'b'];
            for ch in s.chars() {
                let digit = ch.to_digit(16)? as u8;
                buf.push(if digit & 8 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 4 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 2 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 1 != 0 { b'1' } else { b'0' });
            }
            Some(buf)
        }

        // Parse decimal and convert to binary
        vhpi::Value::DecStr(_s) => {
            // let n: u64 = s.parse().ok()?;
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(n, signal_len as usize));
            // Some(buf)
            todo!()
        }

        // Char as its unicode scalar value in binary
        vhpi::Value::Char(c) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*c as u64, 8));
            Some(buf)
        }

        // Integers — store as two's complement bitvec
        vhpi::Value::Int(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 32));
            Some(buf)
        }

        vhpi::Value::LongInt(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 64));
            Some(buf)
        }

        vhpi::Value::SmallPhysical(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 32));
            Some(buf)
        }

        vhpi::Value::Physical(_p) => {
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(p.value as u64, 64));
            // Some(buf)
            todo!()
        }

        // Time — strip unit, encode integer part
        vhpi::Value::Time(_t) => {
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(t.value, 64));
            // Some(buf)
            todo!()
        }

        // Single logic value
        vhpi::Value::Logic(v) => {
            // if signal_len == 1 {
            //     Some(vec![logic_val_to_char(v)])
            // } else {
            //     Some(vec![b'b', logic_val_to_char(v)])
            // }

            Some(vec![logic_val_to_char(v)])
        }

        // Logic vector
        vhpi::Value::LogicVec(vals) => {
            let mut buf = Vec::with_capacity(vals.len() + 1);
            buf.push(b'b');
            for v in vals {
                buf.push(logic_val_to_char(v));
            }
            Some(buf)
        }

        // Enums — encode discriminant as bitvec
        vhpi::Value::SmallEnum(_v) => {
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(*v as u64, signal_len as usize));
            // Some(buf)
            todo!()
        }

        vhpi::Value::Enum(_v) => {
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(*v as u64, signal_len as usize));
            // Some(buf)
            todo!()
        }

        // Real — 'r' prefix
        vhpi::Value::Real(f) => {
            let mut buf = vec![b'r'];
            buf.extend_from_slice(f.to_string().as_bytes());
            Some(buf)
        }

        // String — 's' prefix
        vhpi::Value::Str(s) => {
            let mut buf = Vec::with_capacity(s.len() + 1);
            buf.push(b's');
            buf.extend_from_slice(s.as_bytes());
            Some(buf)
        }

        // Vec types — no good single-signal encoding, caller should split into separate signals
        vhpi::Value::IntVec(_)
        | vhpi::Value::LongIntVec(_)
        | vhpi::Value::SmallPhysicalVec(_)
        | vhpi::Value::PhysicalVec(_)
        | vhpi::Value::TimeVec(_)
        | vhpi::Value::SmallEnumVec(_)
        | vhpi::Value::EnumVec(_)
        | vhpi::Value::RealVec(_) => todo!(),

        vhpi::Value::Unknown => todo!(),
    }
}
