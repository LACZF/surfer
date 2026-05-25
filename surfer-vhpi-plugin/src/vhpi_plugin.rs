use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use portable_atomic::AtomicU128;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use surfer_simulator_protocol::{
    HierarchyElement, SignalChange, SignalValue, SimulatorToSurferMessage,
    SurferToSimulatorMessage, TimeStep,
};
use surfer_time_types::TimeScale;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use vhpi::{CbData, CbReason, Handle, Mode, OneToOne, StrProperty, Time, Value, printf};
use wellen::{ScopeType, SignalEncoding, VarDirection, VarIndex, VarType};

static RECORD_ALL_CHANGES: bool = false;

static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static START_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<Vec<TimeStep>>> = OnceLock::new();
static SIM_TO_TCP_TX: OnceLock<mpsc::UnboundedSender<SimulatorToSurferMessage>> = OnceLock::new();
static LAST_SEND_TIME_MS: AtomicU128 = AtomicU128::new(0);
static CALLBACK_STORE: OnceLock<Arc<CallbackStore>> = OnceLock::new();
static SIMULATION_DONE: AtomicBool = AtomicBool::new(false);

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Clone)]
struct StaticSimData {
    hierarchy: Vec<HierarchyElement>,
    id_to_handle: Arc<HashMap<u32, SafeHandle>>,
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
    callback_from_signal_name: DashMap<u32, SafeHandle>,
}

impl CallbackStore {
    pub fn new() -> Self {
        Self {
            callback_from_signal_name: DashMap::new(),
        }
    }

    pub fn insert_callback(&self, id: u32, handle: Handle) {
        self.callback_from_signal_name
            .insert(id, SafeHandle(Arc::new(Mutex::new(handle))));
    }

    pub fn remove_callback(&self, id: u32) -> Option<SafeHandle> {
        self.callback_from_signal_name
            .remove(&id)
            .map(|(_key, safe_handle)| safe_handle)
    }
}

/// The protocol will use ID's to refer to handles so we need some way to keep track of them
pub struct IdStore {
    next_id: AtomicU32,
    pub id_to_handle: HashMap<u32, SafeHandle>,
}

impl IdStore {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(0),
            id_to_handle: HashMap::new(),
        }
    }

    pub fn create_id(&mut self, handle: Handle) -> u32 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        if RECORD_ALL_CHANGES {
            record_value_change(&handle, id);
            let _ = handle.register_cb(CbReason::ValueChange, move |data| {
                record_value_change(data.obj(), id);
            });
        }

        self.id_to_handle
            .insert(id, SafeHandle(Arc::new(Mutex::new(handle))));
        id
    }
}

/// Get the hierarchy so that it can be sent back to Surfer
fn collect_hierarchy_elements_from_region(
    region: &Handle,
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    scope_type: ScopeType,
) {
    h.push(HierarchyElement::PushScope {
        name: region.get_name().unwrap_or_default(),
        scope_type,
    });

    for port in region.iterator(vhpi::OneToMany::PortDecls) {
        let type_handle = port.handle(vhpi::OneToOne::Type);
        let scope = vec![port.get_name().unwrap_or_default()];
        let var_direction = port.get_mode().map(|dir| match dir {
            Mode::In => VarDirection::Input,
            Mode::Out => VarDirection::Output,
            Mode::Inout => VarDirection::InOut,
            Mode::Buffer => VarDirection::Buffer,
            Mode::Linkage => VarDirection::Linkage,
        });

        add_var_outer(h, id_store, port, &type_handle, scope, var_direction);
    }

    /* TODO: Handle parameters correctly
    for parameter in region.iterator(vhpi::OneToMany::GenericDecls) {
        let type_handle = parameter.handle(vhpi::OneToOne::Type);
        let scope = vec![parameter.get_name().unwrap_or_default()];

        add_var_outer(h, id_store, parameter, &type_handle, scope, None);
    }
    */

    for sig in region.iterator(vhpi::OneToMany::SigDecls) {
        let type_handle = sig.handle(vhpi::OneToOne::Type);
        let scope = vec![sig.get_name().unwrap_or_default()];

        add_var_outer(h, id_store, sig, &type_handle, scope, None);
    }

    for sub in region.iterator(vhpi::OneToMany::InternalRegions) {
        collect_hierarchy_elements_from_region(&sub, h, id_store, ScopeType::Module);
    }

    h.push(HierarchyElement::PopScope);
}

fn add_var_outer(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    sig: Handle,
    type_handle: &Handle,
    parent_scope: Vec<String>,
    var_direction: Option<VarDirection>,
) {
    match type_handle.get_kind() {
        Some(vhpi::ClassKind::ArrayTypeDecl) => {
            add_var_inner(
                h,
                id_store,
                sig,
                type_handle,
                parent_scope,
                None,
                var_direction,
            );
        }
        Some(vhpi::ClassKind::RecordTypeDecl) => {
            let mut my_scope = parent_scope.clone();
            let mut record_name = String::new();
            while let Some(record_name_part) = my_scope.pop() {
                if record_name == sig.get_name().unwrap() {
                    break;
                }
                record_name.insert_str(0, &record_name_part);
            }

            h.push(HierarchyElement::PushScope {
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
                add_var_outer(
                    h,
                    id_store,
                    field,
                    &field_type_handle,
                    my_inner_scope,
                    var_direction,
                );
            }

            h.push(HierarchyElement::PopScope);
        }
        Some(vhpi::ClassKind::EnumTypeDecl) => {
            let enum_map = type_handle.enum_literals().map(|entries| {
                entries
                    .into_iter()
                    .enumerate()
                    .map(|(ordinal, value)| (ordinal.to_string(), value))
                    .collect()
            });
            add_var_inner(
                h,
                id_store,
                sig,
                type_handle,
                parent_scope,
                enum_map,
                var_direction,
            );
        }
        Some(_kind) => {
            add_var_inner(
                h,
                id_store,
                sig,
                type_handle,
                parent_scope,
                None,
                var_direction,
            );
        }
        None => {
            printf!("signal {} with unsupported kind", sig.get_name().unwrap());
        }
    }
}

fn add_var_inner(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    sig: Handle,
    type_handle: &Handle,
    parent_scope: Vec<String>,
    enum_map: Option<Vec<(String, String)>>,
    var_direction: Option<VarDirection>,
) {
    let parse_type = match sig.get_value(vhpi::Format::ObjType) {
        Ok(value) => get_wellen_type(&value),
        Err(err) => {
            printf!(
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

                    add_var_outer(
                        h,
                        id_store,
                        elem,
                        &elem_type_handle,
                        my_scope,
                        var_direction,
                    );
                }
            }
        }
        ParseType::Simple(var_type, signal_encoding) => {
            let mut my_scope = parent_scope;

            let index = get_var_range(type_handle).map(|(msb, lsb)| VarIndex::new(msb, lsb));

            let id = id_store.create_id(sig);

            let name = my_scope.pop().unwrap_or_default();

            let scope = if my_scope.is_empty() {
                None
            } else {
                Some(my_scope.join("."))
            };

            h.push(HierarchyElement::AddVar {
                id,
                name,
                type_name: type_handle.get_name().unwrap_or_default(),
                var_type,
                signal_encoding,
                var_direction,
                enum_map,
                index,
                scope,
            });
        }
        ParseType::Unhandled => todo!(),
    }
}

enum ParseType {
    Simple(VarType, SignalEncoding),
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
        Value::Str(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::Char(_) => ParseType::Simple(VarType::Byte, SignalEncoding::String), // TODO: Is VarType::Byte correct?
        Value::Real(_) => ParseType::Simple(VarType::Real, SignalEncoding::Real),
        Value::Logic(_) => ParseType::Simple(VarType::StdLogic, SignalEncoding::BitVector(1)),
        // Logic vec is handled natively
        Value::LogicVec(vals) => ParseType::Simple(
            VarType::StdLogicVector,
            SignalEncoding::BitVector(vals.len() as u32),
        ),
        Value::BinStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::OctStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::HexStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::DecStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::Int(_) => ParseType::Simple(VarType::Integer, SignalEncoding::BitVector(32)),
        Value::LongInt(_) => ParseType::Simple(VarType::LongInt, SignalEncoding::BitVector(64)),

        // We send the enums as strings by doing a lookup on their value
        Value::Enum(_) => ParseType::Simple(VarType::Enum, SignalEncoding::String),
        Value::SmallEnum(_) => ParseType::Simple(VarType::Enum, SignalEncoding::String),

        // VarType::Time might be a better fit (also for physical?), but I'd rather that Surfer displays the value like an integer so that its human readable
        Value::Time(_) => ParseType::Simple(VarType::LongInt, SignalEncoding::BitVector(64)),
        Value::Physical(_) => ParseType::Simple(VarType::LongInt, SignalEncoding::BitVector(64)),
        Value::SmallPhysical(_) => {
            ParseType::Simple(VarType::Integer, SignalEncoding::BitVector(32))
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
    }
}

/// Gets the range of a type, msb and lsb must be different
fn get_var_range(type_handle: &Handle) -> Option<(i64, i64)> {
    let mut indices = type_handle.index_range().map(|i| i as i64);
    let msb = indices.next()?;
    let lsb = indices.last()?; // returns None if no further elements exist
    Some((msb, lsb))
}
fn timeout_callback(_data: &CbData) {
    // This is called when we want to end the simulation early, either when we pause or our cb_after_delay has been called
    printf!("Timeout reached. Sending values and pausing...");
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
    send_changed_values();

    // Pause the simulator
    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    printf!("Simulation resumed!");
}

fn send_changed_values() {
    // Sends the data over to surfer
    let value_changes = {
        let mut global_vec = VALUE_CHANGES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap();
        std::mem::take(&mut *global_vec)
    };

    // Send the values only if the vector isn't empty
    if !value_changes.is_empty()
        && let Some(tx) = SIM_TO_TCP_TX.get()
    {
        let _ = tx.send(SimulatorToSurferMessage::ValueChanges {
            time_steps: value_changes,
            complete: SIMULATION_DONE.load(Ordering::SeqCst), // Get the true or false value from SIMULATION_DONE
        });
    }
}

/// Callback for the start of simulation, collect the hierarchy and open TCP-connection
fn start_of_sim(_data: &CbData) {
    printf("Start of simulation entered");

    let root = vhpi::handle(vhpi::OneToOne::RootInst);

    // We read the hierarchy here so that it can be sent back
    let mut hierarchy = Vec::new();
    let mut id_store = IdStore::new();
    collect_hierarchy_elements_from_region(&root, &mut hierarchy, &mut id_store, ScopeType::Module);

    // Open communication to surfer
    let runtime = RUNTIME_HANDLE.get().unwrap();
    let bind_address = get_bind_address_from_startup_arguments();
    runtime.spawn(message_loop(
        StaticSimData {
            hierarchy,
            id_to_handle: Arc::new(id_store.id_to_handle),
        },
        bind_address,
    ));

    // Block here until message is received
    printf("Waiting for RunSimulation message...");
    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    printf("Simulation started!");
}

/// This runs every timestep that has a change or something important is happening
/// Send current simulation time to Surfer every 0.5s of wall-clock time.
fn next_time_step(_data: &CbData) {
    let now_ms = now_millis();
    let last_time_ms = LAST_SEND_TIME_MS.load(Ordering::Relaxed);

    // 0.5 seconds in milliseconds
    let interval_ms = 500;

    if now_ms - last_time_ms >= interval_ms {
        LAST_SEND_TIME_MS.store(now_ms, Ordering::Relaxed);
        let sim_time = vhpi::get_time().to_i64();

        if let Some(tx) = SIM_TO_TCP_TX.get() {
            let _ = tx.send(SimulatorToSurferMessage::CurrentSimulationTime { time: sim_time });
        }
    }

    // Register callback for next REAL simulation event
    let _ = vhpi::register_cb(CbReason::NextTimeStep, next_time_step);
}

/// This runs when the simulation is complete, sends all data over to Surfer
fn end_of_sim(_data: &CbData) {
    // Tells the plugin that the simulation is done
    SIMULATION_DONE.store(true, Ordering::SeqCst);
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
    send_changed_values();
    printf!(
        "End of simulation reached. All values sent to Surfer. Plugin will now block indefinitely."
    );
    thread::park(); // blocks indefinitely
}

/// Handles the connection from Surfer
async fn handle_connection(stream: TcpStream, data: StaticSimData) {
    let (read_half, write_half) = stream.into_split();

    let mut reader = FramedRead::new(read_half, LengthDelimitedCodec::new());
    let mut writer = FramedWrite::new(write_half, LengthDelimitedCodec::new());

    let (tx, mut rx) = mpsc::unbounded_channel();
    if SIM_TO_TCP_TX.set(tx).is_err() {
        printf!("SIM_TO_TCP_TX was already set, this connection will be broken!");
    }

    loop {
        tokio::select! {
            // Surfer send a message and we decode what it is
            Some(frame_result) = reader.next() => {
                let frame = match frame_result {
                    Ok(f) => f,
                    Err(_) => break, // End the loop if connection dies
                };

                let msg: SurferToSimulatorMessage = match bincode::deserialize(&frame) {
                    Ok(msg) => msg,
                    Err(err) => {
                        printf!("Failed to decode message from Surfer: {:?}", err);
                        continue;
                    }
                };
                let response: SimulatorToSurferMessage = match msg {
                    SurferToSimulatorMessage::RequestSimulatorInfo => {
                        let name = vhpi::simulator_name().unwrap_or_else(|| "unknown".to_string());
                        let version = vhpi::simulator_version().unwrap_or_else(|| "unknown".to_string());
                        SimulatorToSurferMessage::SimulatorInfo {
                            name,
                            version,
                        }
                    }
                    SurferToSimulatorMessage::RunSimulation { time: maybe_delay } => {
                        if SIMULATION_DONE.load(Ordering::SeqCst) {
                            //Simulation is done so we don't want so simulate any more time
                            SimulatorToSurferMessage::ValueChanges { time_steps: [].to_vec(), complete: true }
                        }
                        else if CALLBACK_STORE.get().is_none_or(|store| store.callback_from_signal_name.is_empty()) {
                            //If we have not chosen any signals we donät want to simulate anything
                            SimulatorToSurferMessage::ValueChanges { time_steps: [].to_vec(), complete: true }
                        }
                        else{
                            if let Some(delay) = maybe_delay {
                                vhpi::register_cb_after_delay(Time::from(delay as i64), timeout_callback).ok();
                            }
                            START_NOTIFY.get().unwrap().notify_one();
                            SimulatorToSurferMessage::Acknowledge { success: true }
                        }
                    }
                    SurferToSimulatorMessage::PauseSimulation => {
                        // Same logic as timeout, but requested from the outside
                        vhpi::register_cb_after_delay(Time::from(0_i64), timeout_callback).ok();
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                    SurferToSimulatorMessage::RequestHierarchy => {
                        let hierarchy = data.hierarchy.clone();
                        let resolution_fs = vhpi::simulator_time_resolution().to_i64();
                        let resolution_fs = if resolution_fs > 0 { resolution_fs as u64 } else { 1 };
                        SimulatorToSurferMessage::Hierarchy {
                            hierarchy,
                            time_scale: TimeScale::from_fs(resolution_fs),
                        }
                    }
                    SurferToSimulatorMessage::GetValueChanges => {
                        let mut global_vec = VALUE_CHANGES
                            .get_or_init(|| Mutex::new(Vec::new()))
                            .lock()
                            .unwrap();
                        let value_changes = std::mem::take(&mut *global_vec);
                        SimulatorToSurferMessage::ValueChanges { time_steps: value_changes, complete: true }
                    }
                    SurferToSimulatorMessage::TrackVariableChanges { id } => {
                        set_callback_on_signal(id, data.id_to_handle.as_ref());
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                    SurferToSimulatorMessage::UntrackVariableChanges { id } => {
                        remove_callback(id);
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                };

                let encoded = match bincode::serialize(&response) {
                    Ok(encoded) => encoded,
                    Err(err) => {
                        printf!("Failed to serialize response to Surfer: {:?}", err);
                        continue;
                    }
                };
                if let Err(err) = writer.send(encoded.into()).await {
                    printf!("Failed to send response to Surfer: {:?}", err);
                    break;
                }
            }

            // If we get told to send a message to surfer, we send that message
            Some(plugin_msg) = rx.recv() => {
                let encoded = match bincode::serialize(&plugin_msg) {
                    Ok(encoded) => encoded,
                    Err(err) => {
                        printf!("Failed to serialize plugin message: {:?}", err);
                        continue;
                    }
                };
                if let Err(err) = writer.send(encoded.into()).await {
                    printf!("Failed to send plugin message to Surfer: {:?}", err);
                    break;
                }
            }
        }
    }
}

async fn message_loop(data: StaticSimData, bind_address: String) {
    //Sets upp the TCP connection and enters the connection loop
    let listener = TcpListener::bind(&bind_address).await.unwrap();

    loop {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::spawn(handle_connection(stream, data.clone()));
    }
}

/// Set up all necessary info so the simulation can start
#[unsafe(no_mangle)]
pub extern "C" fn vhpi_plugin_startup() {
    VALUE_CHANGES.set(Mutex::new(Vec::new())).ok();
    START_NOTIFY.set(Arc::new(Notify::new())).ok();
    CALLBACK_STORE.set(Arc::new(CallbackStore::new())).ok();

    let start_ms = now_millis();
    LAST_SEND_TIME_MS.store(start_ms, Ordering::Relaxed);

    // Set up communication over TCP
    let runtime = tokio::runtime::Runtime::new().unwrap();
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();

    Box::leak(Box::new(runtime));

    let _ = vhpi::register_cb(CbReason::StartOfSimulation, start_of_sim);
    let _ = vhpi::register_cb(CbReason::EndOfSimulation, end_of_sim);
    let _ = vhpi::register_cb(CbReason::NextTimeStep, next_time_step);
}

fn get_bind_address_from_startup_arguments() -> String {
    let default_port: u16 = 6967;
    let tool = vhpi::handle(OneToOne::Tool);

    for argv in tool.iterator(vhpi::OneToMany::Argvs) {
        let argument = argv
            .get_str(StrProperty::StrVal)
            .or_else(|| argv.get_str(StrProperty::Name))
            .unwrap_or_default();

        if let Some(port_str) = argument.strip_prefix('+') {
            if let Ok(port) = port_str.parse::<u16>() {
                let bind_address = format!("127.0.0.1:{port}");
                printf!("Binding simulator plugin to {}", bind_address);
                return bind_address;
            }

            printf!(
                "Ignoring invalid startup argument {:?}, expected +port",
                argument
            );
        }
    }

    printf!(
        "No valid port startup argument found, using default {}",
        default_port
    );
    format!("127.0.0.1:{}", default_port)
}

fn set_callback_on_signal(id: u32, id_to_handle: &HashMap<u32, SafeHandle>) {
    // Function that finds the input signals, puts a value changed callback on it and sends it's start value to the VALUE_CHANGED vector
    let callback_store = CALLBACK_STORE.get().unwrap();
    if callback_store.callback_from_signal_name.contains_key(&id) {
        printf!("Callback on {:?} already exists", id);
    } else {
        let sig_handle = &id_to_handle.get(&id).unwrap().0.lock().unwrap();
        // Lock the inner Handle to register the callback
        let callback_handle = sig_handle
            .register_cb(CbReason::ValueChange, move |data| {
                record_value_change(data.obj(), id)
            })
            .unwrap();
        callback_store.insert_callback(id, callback_handle);
        // Call value_function with a locked reference to the handle
        record_value_change(sig_handle, id);
    }
}

/// Unregister a signal from writing changes
fn remove_callback(sig_id: u32) {
    // Finds the callback
    let callback_store = CALLBACK_STORE.get().unwrap();
    // Removes it from the callback_store and remove the callback in it's entirety
    let callback_handle = callback_store.remove_callback(sig_id).unwrap();
    vhpi::remove_cb(&callback_handle.0.lock().unwrap());

    let mut time_steps = VALUE_CHANGES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    for time_step in time_steps.iter_mut() {
        // Removes all instances of saved values of chosen signal
        time_step.changes.retain(|change| change.id != sig_id);
    }
    // Removes all TimeSteps that becomes empty
    time_steps.retain(|ts| !ts.changes.is_empty());
}

// Finds the value of the signal when ValueChanged has been called and then creates a SignalChanged for that change
fn record_value_change(signal_handle: &Handle, id: u32) {
    let mut global_changes = VALUE_CHANGES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();

    if let Some(value) = vhpi_signal_value_to_vcd_bytes(signal_handle) {
        let current_time = vhpi::get_time().to_i64();

        let change: SignalChange = SignalChange {
            id,
            value: SignalValue::VCDValue(value),
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

fn int_to_bitvec_chars(value: u64, bits: usize) -> Vec<u8> {
    (0..bits)
        .rev()
        .map(|i| if (value >> i) & 1 == 1 { b'1' } else { b'0' })
        .collect()
}

/// Convert the value from VHPI to VCD-bytes that can be sent to surfer
/// Signal handle is passed so that we can access enum-kinds
fn vhpi_signal_value_to_vcd_bytes(sig: &Handle) -> Option<Vec<u8>> {
    // TODO: Time and Physical should be encoded as strings
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

        vhpi::Value::Logic(v) => Some(vec![u8::from(*v)]),

        vhpi::Value::LogicVec(vals) => {
            let mut buf = Vec::with_capacity(vals.len() + 1);
            buf.push(b'b');
            for v in vals {
                buf.push(u8::from(*v));
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
