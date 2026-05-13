use futures::{SinkExt, StreamExt};
use surfer_simulator_protocol::{
    HierarchyElem, PluginToSurferMessage, ScopeType, SignalEncoding, SurferToPluginMessage,
    TimeUnit, VarType,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use surfer_simulator_protocol::SignalChange;
use surfer_simulator_protocol::TimeStep;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use vhpi::{Handle, Value};

// TODO: Split into multiple files, e.g.
// - hierarchy.rs
// - id_store.rs
// - value_change.rs
// - callbacks.rs ??

static RECORD_ALL_CHANGES: bool = false;

static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static START_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<Vec<TimeStep>>> = OnceLock::new();
static SIM_TO_TCP_TX: OnceLock<mpsc::UnboundedSender<PluginToSurferMessage>> = OnceLock::new();
static LAST_SEND_TIME: AtomicI64 = AtomicI64::new(0);
static CALLBACK_STORE: OnceLock<Arc<CallbackStore>> = OnceLock::new();

#[derive(Clone)]
struct StaticSimData {
    hierarchy: Vec<HierarchyElem>,
    id_to_handle: HashMap<u64, SafeHandle>,
}

pub struct SafeHandle(pub Arc<Mutex<Handle>>);
impl Clone for SafeHandle {
    fn clone(&self) -> Self {
        SafeHandle(self.0.clone())
    }
}
unsafe impl Send for SafeHandle {}
unsafe impl Sync for SafeHandle {}

pub struct CallbackStore {
    callback_from_signal_name: DashMap<u64, SafeHandle>,
}

impl CallbackStore {
    pub fn new() -> Self {
        Self {
            callback_from_signal_name: DashMap::new(),
        }
    }

    pub fn insert_callback(&self, id: u64, handle: vhpi::Handle) {
        self.callback_from_signal_name
            .insert(id, SafeHandle(Arc::new(Mutex::new(handle))));
    }

    pub fn remove_callback(&self, id: u64) -> Option<SafeHandle> {
        self.callback_from_signal_name
            .remove(&id)
            .map(|(_key, safe_handle)| safe_handle)
    }
}

// The protocol will use ID's to handle names so we need some way to keep track of them
pub struct IdStore {
    next_id: AtomicU64,
    pub id_to_handle: HashMap<u64, SafeHandle>, // Store SafeHandle instead of String
}

impl IdStore {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            id_to_handle: HashMap::new(),
        }
    }

    pub fn create_id(&mut self, handle: Handle) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        if RECORD_ALL_CHANGES {
            value_function(&handle, id);
            let _ = handle.register_cb(vhpi::CbReason::ValueChange, move |data| {
                value_change(data, id);
            });
        }

        self.id_to_handle
            .insert(id, SafeHandle(Arc::new(Mutex::new(handle))));
        id
    }
}

fn value_change(data: &vhpi::CbData, id: u64) {
    let val = data.obj.get_value(vhpi::Format::ObjType);
    match val {
        Ok(value) => {
            let full_name = data.obj.get_str(vhpi::StrProperty::FullName).unwrap();
            vhpi::printf!("value change {} => {}", full_name, value);

            value_function(&data.obj, id);
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

/// Get the hierarchy so that it can be sent back to Surfer
fn collect_hierarchy_elements_from_region(
    region: &vhpi::Handle,
    h: &mut Vec<HierarchyElem>,
    id_store: &mut IdStore,
) {
    h.push(HierarchyElem::PushScope {
        name: region.get_name().unwrap_or_default(),
        scope_type: ScopeType::VhdlArchitecture, // TODO: This shouldn't be hardcoded?
    });

    for port in region.iterator(vhpi::OneToMany::PortDecls) {
        // TODO
        println!("port {}", port.get_name().unwrap());
        // let _ = port.register_cb(vhpi::CbReason::ValueChange, value_change);
    }

    for sig in region.iterator(vhpi::OneToMany::SigDecls) {
        let type_handle = sig.handle(vhpi::OneToOne::Type);
        let scope = vec![sig.get_name().unwrap_or_default()];

        add_var_outer(h, id_store, sig, &type_handle, scope);
    }

    for sub in region.iterator(vhpi::OneToMany::InternalRegions) {
        println!("internal region {}", sub.get_name().unwrap());
        collect_hierarchy_elements_from_region(&sub, h, id_store);
    }

    h.push(HierarchyElem::PopScope);
}

fn add_var_outer(
    h: &mut Vec<HierarchyElem>,
    id_store: &mut IdStore,
    sig: Handle,
    type_handle: &Handle,
    parent_scope: Vec<String>,
) {
    match type_handle.get_kind() {
        Some(vhpi::ClassKind::ArrayTypeDecl) => {
            add_var_inner(h, id_store, sig, &type_handle, parent_scope);
        }
        Some(vhpi::ClassKind::RecordTypeDecl) => {
            // TODO: We aren't handling record types correctly, tb_complex is a good example

            // TODO: Do we need a scope for a record?
            // println!(
            //     "signal {} is a record of type {}",
            //     sig.get_name().unwrap(),
            //     type_handle.get_name().unwrap()
            // );
            let mut my_scope = parent_scope.clone();
            let mut record_name = String::new();
            while let Some(record_name_part) = my_scope.pop() {
                if record_name == sig.get_name().unwrap() {
                    break;
                }
                record_name.insert_str(0, &record_name_part);
            }

            h.push(HierarchyElem::PushScope {
                name: record_name,
                scope_type: ScopeType::VhdlRecord,
            });

            for field in sig.iterator(vhpi::OneToMany::SelectedNames) {
                let mut my_inner_scope = my_scope.clone();

                let including_parent_name = field.get_name().unwrap_or_default();
                let field_name = including_parent_name
                    .rsplit_once('.')
                    .unwrap()
                    .1
                    .to_string();

                my_inner_scope.push(field_name);

                let field_type_handle = field.handle(vhpi::OneToOne::Type);
                add_var_outer(h, id_store, field, &field_type_handle, my_inner_scope);
            }

            h.push(HierarchyElem::PopScope);
        }
        Some(vhpi::ClassKind::EnumTypeDecl) => {
            // TODO: Add proper support for enums
            dbg!(type_handle.enum_literals().unwrap_or_default());

            add_var_inner(h, id_store, sig, &type_handle, parent_scope);
        }
        Some(_kind) => {
            add_var_inner(h, id_store, sig, &type_handle, parent_scope);
        }
        None => {
            println!("signal {} with unsupported kind", sig.get_name().unwrap());
        }
    }
}

fn add_var_inner(
    h: &mut Vec<HierarchyElem>,
    id_store: &mut IdStore,
    sig: Handle,
    type_handle: &Handle,
    parent_scope: Vec<String>,
) {
    let parse_type = match sig.get_value(vhpi::Format::ObjType) {
        Ok(value) => get_wellen_type(&value),
        Err(err) => {
            vhpi::printf!(
                "Failed to get value of {:?}, err: {:?}",
                sig.get_name().unwrap(),
                err
            );
            ParseType::ArrayType
        }
    };

    match parse_type {
        ParseType::ArrayType => {
            let elem_type_handle = type_handle.handle(vhpi::OneToOne::ElemType);
            for (index, i) in type_handle.index_range().enumerate() {
                if let Some(elem) = sig.handle_by_index(vhpi::OneToMany::IndexedNames, index as i32)
                {
                    let mut my_scope = parent_scope.clone();
                    my_scope.push(format!("[{i}]"));

                    add_var_outer(h, id_store, elem, &elem_type_handle, my_scope);
                }
            }
        }
        ParseType::Simple(var_type, signal_encoding, length) => {
            let mut my_scope = parent_scope;

            if matches!(var_type, VarType::StdLogicVector | VarType::String) {
                if let Some((msb, lsb)) = get_var_range(type_handle) {
                    my_scope.push(format!("[{msb}:{lsb}]"));
                }
            }

            let id = id_store.create_id(sig);

            h.push(HierarchyElem::AddVar {
                id: id,
                name: my_scope.join(""),
                type_name: type_handle.get_name().unwrap_or_default(),
                length: length,
                var_type: var_type,
                signal_encoding: signal_encoding,
            });
        }
        ParseType::Unhandled => todo!(),
    }
}

// TODO: Consider creating signal encoding Surfer-side
enum ParseType {
    Simple(VarType, SignalEncoding, u32),
    ArrayType,
    Unhandled,
}

/// Gets the matching wellen type info for a VHPI value
/// Note that vector types other than LogicVec should be split into composite signals
fn get_wellen_type(value: &Value) -> ParseType {
    // This is split into 3 cases:
    //  1. A value can be directly translated to a wellen type
    //  2. A value needs to be split into component parts, i.e. IntVec, we will need to handle children as their own scope recursively
    //  3. Unhandled, for stuff that we have no idea what to do with need to prevent infinite recursion

    match value {
        Value::Str(_) => ParseType::Simple(VarType::String, SignalEncoding::String, 0),
        Value::Char(_) => ParseType::Simple(VarType::Byte, SignalEncoding::String, 0), // Is this right???
        Value::Real(_) => ParseType::Simple(VarType::Real, SignalEncoding::Real, 64),
        Value::Logic(_) => {
            ParseType::Simple(VarType::StdLogic, SignalEncoding::bit_vec_of_len(1), 1)
        }
        // Logic vec is handled natively
        Value::LogicVec(vals) => ParseType::Simple(
            VarType::StdLogicVector,
            SignalEncoding::bit_vec_of_len(vals.len() as u32),
            vals.len() as u32,
        ),
        Value::BinStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String, 0),
        Value::OctStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String, 0),
        Value::HexStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String, 0),
        Value::DecStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String, 0),
        Value::Int(_) => {
            ParseType::Simple(VarType::Integer, SignalEncoding::bit_vec_of_len(32), 32)
        }
        Value::LongInt(_) => {
            ParseType::Simple(VarType::LongInt, SignalEncoding::bit_vec_of_len(64), 64)
        }

        // VarType::Time might be a better fit, but I'd rather that Surfer displays the value like an integer
        Value::Time(_) => {
            ParseType::Simple(VarType::LongInt, SignalEncoding::bit_vec_of_len(64), 64)
        }

        // We send the enums as strings by doing a lookup on their value
        Value::Enum(_) => ParseType::Simple(VarType::Enum, SignalEncoding::String, 0),
        Value::SmallEnum(_) => ParseType::Simple(VarType::Enum, SignalEncoding::String, 0),

        // Time seems to be sort of a subtype of physical so I think this makes sense?
        Value::Physical(_) => {
            ParseType::Simple(VarType::LongInt, SignalEncoding::bit_vec_of_len(64), 64)
        }
        Value::SmallPhysical(_) => {
            ParseType::Simple(VarType::Integer, SignalEncoding::bit_vec_of_len(32), 32)
        }

        Value::IntVec(_) => ParseType::ArrayType,
        Value::EnumVec(_) => ParseType::ArrayType,
        Value::RealVec(_) => ParseType::ArrayType,
        Value::TimeVec(_) => ParseType::ArrayType,
        Value::LongIntVec(_) => ParseType::ArrayType,
        Value::PhysicalVec(_) => ParseType::ArrayType,
        Value::SmallEnumVec(_) => ParseType::ArrayType,
        Value::SmallPhysicalVec(_) => ParseType::ArrayType,

        Value::Unknown => ParseType::Unhandled,
        // _ => todo!("Unhandled variant {:?}", value),
    }
}

/// Gets the range of a type, msb and lsb must be different
fn get_var_range(type_handle: &Handle) -> Option<(i64, i64)> {
    let mut indices = type_handle.index_range().map(|i| i as i64);
    let msb = indices.next()?;
    let lsb = indices.last()?; // returns None if no further elements exist
    Some((msb, lsb))
}
fn timeout_callback(_data: &vhpi::CbData) {
    vhpi::printf!("Timeout reached. Sending values and pausing...");
    {
        let mut value_changes = VALUE_CHANGES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap();

        // We need some way to inform Surfer when the simulation is done
        value_changes.push(TimeStep {
            timestamp: vhpi::get_time().to_i64(),
            changes: vec![],
        });
    }
    send_changed_values(false);

    // 3. Pausa simulatorn
    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    vhpi::printf!("Simulation resumed!");
}

fn send_changed_values(is_finished: bool) {
    let value_changes = {
        let mut global_vec = VALUE_CHANGES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap();
        std::mem::take(&mut *global_vec)
    };

    // Send the values only if the vector isn't empty
    if !value_changes.is_empty() {
        if let Some(tx) = SIM_TO_TCP_TX.get() {
            let _ = tx.send(PluginToSurferMessage::SendChangedValues(
                value_changes,
                is_finished,
            ));
            //std::thread::sleep(std::time::Duration::from_millis(2000));
        }
    }
}

fn start_of_sim(_data: &vhpi::CbData) {
    vhpi::printf("start of simulation");

    let root = vhpi::handle(vhpi::OneToOne::RootInst);

    vhpi::printf!("root name is {}", root.get_name().unwrap());
    vhpi::printf!("root kind is {:?}", root.get_kind());

    // We read the hierarchy here so that it can be sent back
    let mut hierarchy = Vec::new();
    let mut id_store = IdStore::new();
    collect_hierarchy_elements_from_region(&root, &mut hierarchy, &mut id_store);

    let runtime = RUNTIME_HANDLE.get().unwrap();

    runtime.spawn(message_loop(StaticSimData {
        hierarchy: hierarchy,
        id_to_handle: id_store.id_to_handle,
    }));

    // Block here until message is received
    vhpi::printf("waiting for RunSimulation message...");
    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    vhpi::printf("simulation started!");
}

fn next_time_step(_data: &vhpi::CbData) {
    let current_time = vhpi::get_time().to_i64();
    let last_time = LAST_SEND_TIME.load(Ordering::Relaxed);

    // 500_000_000_000_000 fs = 0.5 seconds
    let interval_fs = 500_000_000_000_000;

    if current_time - last_time >= interval_fs {
        LAST_SEND_TIME.store(current_time, Ordering::Relaxed);

        let time_seconds: f64 = (current_time as f64) / 1_000_000_000_000_000.0;
        if let Some(tx) = SIM_TO_TCP_TX.get() {
            let _ = tx.send(PluginToSurferMessage::SendUpdatedTime(time_seconds));
        }
    }

    // Register callback for next REAL simulation event
    let _ = vhpi::register_cb(vhpi::CbReason::NextTimeStep, next_time_step);
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
    {
        let mut value_changes = VALUE_CHANGES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap();

        // We need some way to inform Surfer when the simulation is done
        value_changes.push(TimeStep {
            timestamp: vhpi::get_time().to_i64(),
            changes: vec![],
        });
    }
    send_changed_values(true);

    thread::park(); // blocks indefinitely
}

async fn handle_connection(stream: TcpStream, data: StaticSimData) {
    let (read_half, write_half) = stream.into_split();

    let mut reader = FramedRead::new(read_half, LengthDelimitedCodec::new());
    let mut writer = FramedWrite::new(write_half, LengthDelimitedCodec::new());

    let (tx, mut rx) = mpsc::unbounded_channel();
    if SIM_TO_TCP_TX.set(tx).is_err() {
        dbg!("[plugin] SIM_TO_TCP_TX was already set — this connection will be broken!");
    }

    loop {
        tokio::select! {
            // Surfer send a message and we decode what it is
            Some(frame_result) = reader.next() => {
                let frame = match frame_result {
                    Ok(f) => f,
                    Err(_) => break, // End the loop if connection dies
                };

                let msg: SurferToPluginMessage = bincode::deserialize(&frame).unwrap();
                let response: PluginToSurferMessage = match msg {
                    SurferToPluginMessage::GetSimulatorName => {
                        let name = vhpi::simulator_name().unwrap();
                        PluginToSurferMessage::SimulatorName(name)
                    }
                    SurferToPluginMessage::RunSimulation(maybe_delay) => {
                        if let Some(delay) = maybe_delay {
                            vhpi::register_cb_after_delay(vhpi::Time::from(delay as i64), timeout_callback).ok();
                        }
                        START_NOTIFY.get().unwrap().notify_one();
                        PluginToSurferMessage::Acknowledge(true)
                    }
                    SurferToPluginMessage::PauseSimulation => {
                        // Same logic as timeout, but requested from the outside
                        vhpi::register_cb_after_delay(vhpi::Time::from(0 as i64), timeout_callback).ok();
                        PluginToSurferMessage::Acknowledge(true)
                    }
                    SurferToPluginMessage::GetHierarchy => {
                        let hierarchy = data.hierarchy.clone();
                        PluginToSurferMessage::Hierarchy(hierarchy, TimeUnit::FemtoSeconds)
                    }
                    SurferToPluginMessage::SendValues => {
                        let mut global_vec = VALUE_CHANGES
                            .get_or_init(|| Mutex::new(Vec::new()))
                            .lock()
                            .unwrap();
                        let value_changes = std::mem::take(&mut *global_vec);
                        PluginToSurferMessage::SendChangedValues(value_changes, true)
                    }
                    SurferToPluginMessage::SetCallback(id) => {
                        set_callback_on_signal(id, data.id_to_handle.clone());
                        PluginToSurferMessage::Acknowledge(true)
                    }
                    SurferToPluginMessage::RemoveCallback(id) => {
                        remove_callback(id);
                        PluginToSurferMessage::Acknowledge(true)
                    }
                    _ => {
                        dbg!(&msg);
                        PluginToSurferMessage::Acknowledge(false)
                    }
                };

                let encoded = bincode::serialize(&response).unwrap();
                writer.send(encoded.into()).await.unwrap();
            }

            // If we get told to send a message to surfer, we send that message
            Some(plugin_msg) = rx.recv() => {
                println!("{:?}", plugin_msg);
                let encoded = bincode::serialize(&plugin_msg).unwrap();
                writer.send(encoded.into()).await.unwrap();
            }
        }
    }
}

async fn message_loop(data: StaticSimData) {
    let listener = TcpListener::bind("0.0.0.0:6967").await.unwrap();

    loop {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::spawn(handle_connection(stream, data.clone()));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nvc_plugin_startup() {
    // TODO: Program exits when simulation is done. We should control it from the message loop part? (Or at least notify Surfer that the process exited)

    VALUE_CHANGES.set(Mutex::new(Vec::new())).ok();
    START_NOTIFY.set(Arc::new(Notify::new())).ok();
    CALLBACK_STORE.set(Arc::new(CallbackStore::new())).ok();

    // Set up communication over TCP
    let runtime = tokio::runtime::Runtime::new().unwrap();
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();

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
    //let _ = vhpi::register_cb_after_delay(vhpi::Time::from(0 as i64), timely_send);
}

fn set_callback_on_signal(id: u64, id_to_handle: HashMap<u64, SafeHandle>) {
    //Function that finds the input signals, puts a value changed callback on it and sends it's start value to the VALUE_CHANGED vector
    println!("SignalsID: {}", id);
    let callback_store = CALLBACK_STORE.get().unwrap();
    if callback_store.callback_from_signal_name.contains_key(&id) {
        println!("Callback on {:?} already exists", id);
    } else {
        let sig_handle = &id_to_handle.get(&id).unwrap().0.lock().unwrap();
        // Lock the inner Handle to register the callback
        let callback_handle = sig_handle
            .register_cb(vhpi::CbReason::ValueChange, move |data| {
                value_change(data, id)
            })
            .unwrap();
        callback_store.insert_callback(id, callback_handle);
        // Call value_function with a locked reference to the handle
        value_function(sig_handle, id);
    }
}

fn remove_callback(sig_id: u64) {
    //Finds the callback
    let callback_store = CALLBACK_STORE.get().unwrap();
    //Removes it from the callback_store and remove the callback in it's entirety
    let callback_handle = callback_store.remove_callback(sig_id).unwrap();
    vhpi::remove_cb(&*callback_handle.0.lock().unwrap());

    let mut time_steps = VALUE_CHANGES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    for time_step in time_steps.iter_mut() {
        //Removes all instances of saved values of chosen signal
        time_step
            .changes
            .retain(|change| change.id != sig_id.into());
    }
    //Removes all TimeSteps that becomes empty
    time_steps.retain(|ts| !ts.changes.is_empty());
}

// Finds the value of the signal when ValueChanged has been called and then creates a SignalChanged for that change
#[allow(unused)]
fn value_function(signal_handle: &Handle, id: u64) {
    let mut global_changes = VALUE_CHANGES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();

    if let Some(sig_value) = vhpi_signal_value_to_vcd_bytes(signal_handle) {
        let current_time = vhpi::get_time().to_i64();

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
}

//TODO: Send all value changes continuously
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

// TODO: Time and Physical should be encoded as strings
fn vhpi_signal_value_to_vcd_bytes(sig: &vhpi::Handle) -> Option<Vec<u8>> {
    let value = &sig.get_value(vhpi::Format::ObjType).ok()?;

    match value {
        // TODO: Validate that this is the right approach
        vhpi::Value::BinStr(s) => {
            let mut buf = Vec::with_capacity(s.len() + 1);
            buf.push(b'b');
            buf.extend_from_slice(s.as_bytes());
            Some(buf)
        }

        // TODO: Validate that this is the right approach
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

        // TODO: Validate that this is the right approach
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

        // TODO: Validate that this is the right approach
        // Parse decimal and convert to binary
        vhpi::Value::DecStr(_s) => {
            // let n: u64 = s.parse().ok()?;
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(n, signal_len as usize));
            // Some(buf)
            todo!()
        }

        // Char is really just a kind of enum, I think?
        vhpi::Value::Char(c) => {
            let mut buf = vec![b's'];
            buf.extend_from_slice(
                sig.handle(vhpi::OneToOne::Type)
                    .enum_literals()
                    .unwrap()
                    .get(*c as usize)
                    .unwrap()
                    .as_bytes(),
            );
            Some(buf)
        }

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

        // TODO: Make human readable, but how?
        vhpi::Value::SmallPhysical(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 32));
            Some(buf)
        }

        // TODO: Make human readable, but how?
        vhpi::Value::Physical(p) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(p.to_i64() as u64, 64));
            Some(buf)
        }

        // TODO: Make human readable, but how?
        vhpi::Value::Time(t) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(t.to_i64() as u64, 64));
            Some(buf)
        }

        vhpi::Value::Logic(v) => Some(vec![logic_val_to_char(v)]),

        vhpi::Value::LogicVec(vals) => {
            let mut buf = Vec::with_capacity(vals.len() + 1);
            buf.push(b'b');
            for v in vals {
                buf.push(logic_val_to_char(v));
            }
            Some(buf)
        }

        vhpi::Value::SmallEnum(v) => {
            let mut buf = vec![b's'];
            buf.extend_from_slice(
                sig.handle(vhpi::OneToOne::Type)
                    .enum_literals()
                    .unwrap()
                    .get(*v as usize)
                    .unwrap()
                    .as_bytes(),
            );
            Some(buf)
        }

        vhpi::Value::Enum(v) => {
            let mut buf = vec![b's'];
            buf.extend_from_slice(
                sig.handle(vhpi::OneToOne::Type)
                    .enum_literals()
                    .unwrap()
                    .get(*v as usize)
                    .unwrap()
                    .as_bytes(),
            );
            Some(buf)
        }

        vhpi::Value::Real(f) => {
            let mut buf = vec![b'r'];
            buf.extend_from_slice(f.to_string().as_bytes());
            Some(buf)
        }

        vhpi::Value::Str(s) => {
            let mut buf = Vec::with_capacity(s.len() + 1);
            buf.push(b's');
            buf.extend_from_slice(s.as_bytes());
            Some(buf)
        }

        // These don't have single-signal encodings, they should split into separate signals in the hierarchy
        vhpi::Value::IntVec(_)
        | vhpi::Value::LongIntVec(_)
        | vhpi::Value::SmallPhysicalVec(_)
        | vhpi::Value::PhysicalVec(_)
        | vhpi::Value::TimeVec(_)
        | vhpi::Value::SmallEnumVec(_)
        | vhpi::Value::EnumVec(_)
        | vhpi::Value::RealVec(_) => None,

        vhpi::Value::Unknown => None,
    }
}
