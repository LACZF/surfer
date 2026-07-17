use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use portable_atomic::AtomicU128;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use surfer_simulator_protocol::{
    HierarchyElement, PROTOCOL_VERSION, SignalChange, SignalValue, SimulationStatus,
    SimulatorToSurferMessage, SurferToSimulatorMessage, TimeStep,
    decode_surfer_to_simulator_message, encode_simulator_to_surfer_message,
};
use surfer_time_types::TimeScale;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use vpi::{CbData, CbReason, Control, Direction, Handle, ObjectType, Time, Value, ValueType};
use wellen::{ScopeType, SignalEncoding, VarDirection, VarIndex, VarType};

static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static START_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<HashMap<u64, Vec<SignalChange>>>> = OnceLock::new();
static LAST_SEND_TIME_MS: AtomicU128 = AtomicU128::new(0);
static LAST_NEXTSIM_CB_TIME: AtomicU64 = AtomicU64::new(u64::MAX);
static CALLBACK_STORE: OnceLock<Arc<CallbackStore>> = OnceLock::new();
static SIMULATION_DONE: AtomicBool = AtomicBool::new(false);
static SIMULATION_PAUSED: AtomicBool = AtomicBool::new(false);

// Icarus sets name for unnamed objects to "unnamed", so we use that as fallback name.
const UNNAMED: &str = "unnamed";

// Time in ms between when plugin sends simulation time update to Surfer.
const INTERVAL_MS: u128 = 500;

struct ClientConnection {
    outgoing: mpsc::UnboundedSender<SimulatorToSurferMessage>,
}

impl ClientConnection {
    fn new(outgoing: mpsc::UnboundedSender<SimulatorToSurferMessage>) -> Self {
        Self { outgoing }
    }

    fn send(&self, message: SimulatorToSurferMessage) {
        let _ = self.outgoing.send(message);
    }
}

static CLIENT_CONNECTION: OnceLock<ClientConnection> = OnceLock::new();

#[inline]
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

#[derive(Clone, Copy)]
pub struct SafeHandle(pub usize);
impl SafeHandle {
    fn from_handle(handle: Handle) -> Self {
        Self(handle.as_raw() as usize)
    }

    fn raw_handle(&self) -> Handle {
        Handle::from_raw(self.0 as *mut u32)
    }
}

pub struct CallbackStore {
    callback_from_signal_id: DashMap<u32, SafeHandle>,
}

impl CallbackStore {
    pub fn new() -> Self {
        Self {
            callback_from_signal_id: DashMap::new(),
        }
    }

    pub fn insert_callback(&self, id: u32, handle: Handle) {
        self.callback_from_signal_id
            .insert(id, SafeHandle::from_handle(handle));
    }

    pub fn remove_callback(&self, id: u32) -> Option<SafeHandle> {
        self.callback_from_signal_id
            .remove(&id)
            .map(|(_key, safe_handle)| safe_handle)
    }
}

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

        self.id_to_handle
            .insert(id, SafeHandle::from_handle(handle));
        id
    }
}

#[inline]
fn current_sim_time() -> u64 {
    vpi::current_simulation_time().to_u64().unwrap_or(0)
}

#[inline]
fn sim_time_from_callback(cb_time: Option<u64>) -> u64 {
    cb_time.unwrap_or_else(current_sim_time)
}

fn infer_var_type_and_encoding(signal: &Handle, fallback_bits: u32) -> (VarType, SignalEncoding) {
    let fallback_bits = fallback_bits.max(1);

    match signal.get_type() {
        Some(ObjectType::Port | ObjectType::PortBit) => {
            return (VarType::Port, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::Net | ObjectType::NetBit) => {
            return (VarType::Wire, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::Reg | ObjectType::RegBit) => {
            return (VarType::Reg, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::NamedEvent) => {
            return (VarType::Event, SignalEncoding::BitVector(0));
        }
        Some(ObjectType::IntVar) => return (VarType::Int, SignalEncoding::BitVector(32)),
        Some(ObjectType::IntegerVar) => {
            return (VarType::Integer, SignalEncoding::BitVector(32));
        }
        Some(ObjectType::ShortIntVar) => {
            return (VarType::ShortInt, SignalEncoding::BitVector(16));
        }
        Some(ObjectType::LongIntVar) => {
            return (VarType::LongInt, SignalEncoding::BitVector(64));
        }
        Some(ObjectType::TimeVar) => {
            return (VarType::Time, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::ByteVar) => return (VarType::Byte, SignalEncoding::BitVector(8)),
        Some(ObjectType::BitVar) => {
            let var_type = if fallback_bits == 1 {
                VarType::Bit
            } else {
                VarType::BitVector
            };
            return (var_type, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::EnumVar) => {
            return (VarType::Enum, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::StringVar) => return (VarType::String, SignalEncoding::String),
        Some(ObjectType::ShortRealVar) => return (VarType::ShortReal, SignalEncoding::Real),
        Some(ObjectType::RealVar) => return (VarType::Real, SignalEncoding::Real),
        Some(ObjectType::MemoryWord) => {
            // Many simulators report memory words via ObjType values as Int,
            // even when the declared element width is not 32. Prefer width-based
            // vector typing for memory elements to preserve declared bit widths.
            let var_type = if fallback_bits == 1 {
                VarType::Bit
            } else {
                VarType::BitVector
            };
            return (var_type, SignalEncoding::BitVector(fallback_bits));
        }
        _ => {}
    }

    match signal.get_value(ValueType::ObjType) {
        Some(Value::Scalar(_)) => (VarType::Wire, SignalEncoding::BitVector(1)),
        Some(Value::Vector(vals)) => {
            let bits = vals.len().max(1) as u32;
            (VarType::Wire, SignalEncoding::BitVector(bits))
        }
        Some(Value::Real(_) | Value::ShortReal(_)) => (VarType::Real, SignalEncoding::Real),
        Some(Value::String(_)) => (VarType::String, SignalEncoding::String),
        Some(Value::Int(_)) => {
            if fallback_bits == 32 {
                (VarType::Int, SignalEncoding::BitVector(32))
            } else {
                (VarType::Wire, SignalEncoding::BitVector(fallback_bits))
            }
        }
        Some(Value::ShortInt(_)) => {
            if fallback_bits == 16 {
                (VarType::ShortInt, SignalEncoding::BitVector(16))
            } else {
                (VarType::Wire, SignalEncoding::BitVector(fallback_bits))
            }
        }
        Some(Value::LongInt(_)) => {
            if fallback_bits == 64 {
                (VarType::LongInt, SignalEncoding::BitVector(64))
            } else {
                (VarType::Wire, SignalEncoding::BitVector(fallback_bits))
            }
        }
        Some(Value::Time(_)) => (VarType::Time, SignalEncoding::BitVector(fallback_bits)),
        #[cfg(feature = "verilator")]
        Some(Value::RawTwoState(bits)) => {
            (VarType::Wire, SignalEncoding::BitVector(bits.len() as u32))
        }
        #[cfg(feature = "verilator")]
        Some(Value::RawFourState(bits)) => {
            (VarType::Wire, SignalEncoding::BitVector(bits.len() as u32))
        }
        _ => (VarType::Wire, SignalEncoding::BitVector(fallback_bits)),
    }
}

fn add_signal_var_named(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    signal: Handle,
    name: String,
) {
    let type_name = signal
        .get_type_name()
        .unwrap_or_else(|| "Unknown".to_string());

    // Icarus errors on vpiDirection for non-port objects; only ask direction on ports.
    let direction = if signal.is_port() {
        match signal.get_direction().as_ref() {
            Some(Direction::Input) => Some(VarDirection::Input),
            Some(Direction::Output) => Some(VarDirection::Output),
            Some(Direction::Inout) => Some(VarDirection::InOut),
            _ => None,
        }
    } else {
        None
    };
    let bits = signal.get_size().unwrap_or(1).max(1);
    let (var_type, signal_encoding) = infer_var_type_and_encoding(&signal, bits);

    let index = match &signal_encoding {
        SignalEncoding::BitVector(width) if *width > 1 => {
            Some(VarIndex::new(i64::from(*width) - 1, 0))
        }
        _ => None,
    };

    let id = id_store.create_id(signal);

    h.push(HierarchyElement::AddVar {
        id,
        name,
        type_name,
        var_type,
        signal_encoding,
        var_direction: direction,
        enum_map: None,
        index,
        scope: None,
    });
}

#[inline]
fn add_signal_var(h: &mut Vec<HierarchyElement>, id_store: &mut IdStore, signal: Handle) {
    let name = signal.get_name().unwrap_or_else(|| UNNAMED.to_string());
    add_signal_var_named(h, id_store, signal, name);
}

#[inline]
fn signal_display_name(signal: &Handle) -> Option<String> {
    signal
        .get_name()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && name != UNNAMED)
}

#[inline]
fn get_signal_full_name(signal: &Handle) -> Option<String> {
    signal
        .get_full_name()
        .or_else(|| signal_display_name(signal))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && name != UNNAMED)
}

#[inline]
fn is_array_object_type(object_type: Option<ObjectType>) -> bool {
    matches!(
        object_type,
        Some(
            ObjectType::Memory
                | ObjectType::RegArray
                | ObjectType::NetArray
                | ObjectType::NamedEventArray
                | ObjectType::PackedArrayNet
                | ObjectType::PackedArrayVar
        )
    )
}

fn infer_ranges_from_name(base_full_name: &str, child_count: usize) -> Vec<(i32, i32)> {
    if child_count == 0 {
        return Vec::new();
    }

    // Determine how many unpacked index brackets are addressable via handle-by-name.
    let mut depth = 1usize;
    while depth <= 8 {
        let probe = format!("{base_full_name}{}", "[0]".repeat(depth));
        if Handle::handle_by_name(&probe).is_null() {
            break;
        }
        depth += 1;
    }
    let max_depth = depth.saturating_sub(1);
    if max_depth <= 1 {
        return Vec::new();
    }

    let mut extents = Vec::with_capacity(max_depth);
    for dim in 0..max_depth {
        let mut idx = 0usize;
        loop {
            let mut name = String::from(base_full_name);
            for d in 0..max_depth {
                let value = if d == dim { idx } else { 0 };
                name.push_str(&format!("[{value}]"));
            }
            if Handle::handle_by_name(&name).is_null() {
                break;
            }
            idx += 1;
            if idx > child_count {
                break;
            }
        }

        if idx == 0 {
            return Vec::new();
        }
        extents.push(idx);
    }

    let product = extents.iter().copied().try_fold(1usize, usize::checked_mul);
    if product != Some(child_count) {
        return Vec::new();
    }

    extents
        .into_iter()
        .map(|extent| (0, extent.saturating_sub(1) as i32))
        .collect()
}

fn array_ranges(signal: &Handle) -> Vec<(i32, i32)> {
    let mut ranges: Vec<(i32, i32)> = signal
        .iterator(ObjectType::Range)
        .filter_map(|range| {
            let left = range.get_left_range()?;
            let right = range.get_right_range()?;
            Some((left, right))
        })
        .collect();

    if !ranges.is_empty() {
        return ranges;
    }

    // Some simulators expose only a direct vpiRange relation (and potentially
    // chained range relations) rather than returning range iterators.
    let mut current = signal.get(ObjectType::Range);
    let mut guard = 0usize;
    while !current.is_null() && guard < 16 {
        if let (Some(left), Some(right)) = (current.get_left_range(), current.get_right_range()) {
            ranges.push((left, right));
        }
        current = current.get(ObjectType::Range);
        guard += 1;
    }

    if !ranges.is_empty() {
        return ranges;
    }

    // Final fallback: derive unpacked dimensions from SystemVerilog type metadata.
    // For arrays like logic [3:0][1:0][7:0] mixed_3d [0:1][0:2], the Memory handle
    // may expose only flattened MemoryWord children; unpacked dimensions can still
    // be represented as ArrayTypespec nodes in the ElemTypespec chain.
    let mut typespec = signal.get(ObjectType::Typespec);
    let mut type_guard = 0usize;
    while !typespec.is_null() && type_guard < 32 {
        if matches!(typespec.get_type(), Some(ObjectType::ArrayTypespec)) {
            let mut type_ranges: Vec<(i32, i32)> = typespec
                .iterator(ObjectType::Range)
                .filter_map(|range| {
                    let left = range.get_left_range()?;
                    let right = range.get_right_range()?;
                    Some((left, right))
                })
                .collect();

            if type_ranges.is_empty() {
                let direct_type_range = typespec.get(ObjectType::Range);
                if let (Some(left), Some(right)) = (
                    direct_type_range.get_left_range(),
                    direct_type_range.get_right_range(),
                ) {
                    type_ranges.push((left, right));
                }
            }

            ranges.extend(type_ranges);
        }

        let next = typespec.get(ObjectType::ElemTypespec);
        if next.is_null() {
            break;
        }
        typespec = next;
        type_guard += 1;
    }

    ranges
}

fn linear_index_to_dim_indices(linear_index: usize, ranges: &[(i32, i32)]) -> Option<Vec<i32>> {
    if ranges.is_empty() {
        return None;
    }

    let extents: Vec<usize> = ranges
        .iter()
        .map(|(left, right)| (*left - *right).unsigned_abs() as usize + 1)
        .collect();

    let total_elems = extents
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)?;
    if linear_index >= total_elems {
        return None;
    }

    let mut remaining = linear_index;
    let mut indices = Vec::with_capacity(ranges.len());

    for dim in 0..ranges.len() {
        let stride = extents[dim + 1..]
            .iter()
            .copied()
            .fold(1usize, usize::saturating_mul);
        let offset = (remaining.checked_div(stride).unwrap_or(0)) % extents[dim];

        let (left, right) = ranges[dim];
        let index = if left <= right {
            left + offset as i32
        } else {
            left - offset as i32
        };
        indices.push(index);

        if stride > 0 {
            remaining %= stride;
        }
    }

    Some(indices)
}

fn array_child_display_name(
    child: &Handle,
    fallback_index: usize,
    parent_ranges: &[(i32, i32)],
) -> String {
    let linear_index = child
        .get_index()
        .and_then(|index| usize::try_from(index).ok())
        .unwrap_or(fallback_index);

    if let Some(dim_indices) = linear_index_to_dim_indices(linear_index, parent_ranges) {
        let mut name = String::new();
        for idx in dim_indices {
            let _ = write!(&mut name, "[{idx}]");
        }
        name
    } else {
        format!("[{linear_index}]")
    }
}

fn preferred_callback_value_type(signal: &Handle) -> ValueType {
    let bits = signal.get_size().unwrap_or(1).max(1);
    let object_type = signal.get_type();
    let inferred_from_type = match object_type {
        // These do not work on Icarus Verilog, so we comment them out for now. They may work on other simulators.
        // Some(ObjectType::ShortIntVar) => Some(ValueType::ShortInt),
        // Some(ObjectType::LongIntVar) => Some(ValueType::LongInt),
        Some(ObjectType::ShortRealVar) => Some(ValueType::ShortReal),
        Some(ObjectType::RealVar) => Some(ValueType::Real),
        Some(ObjectType::StringVar) => Some(ValueType::String),
        Some(ObjectType::IntVar | ObjectType::IntegerVar) => Some(ValueType::Int),
        Some(ObjectType::TimeVar) => Some(ValueType::Time),
        Some(ObjectType::ByteVar) => Some(ValueType::Vector),
        Some(
            ObjectType::Port
            | ObjectType::PortBit
            | ObjectType::Net
            | ObjectType::NetBit
            | ObjectType::Reg
            | ObjectType::RegBit
            | ObjectType::Bit
            | ObjectType::BitVar
            | ObjectType::EnumVar
            | ObjectType::NamedEvent
            | ObjectType::MemoryWord
            | ObjectType::PackedArrayNet
            | ObjectType::PackedArrayVar
            | ObjectType::PartSelect
            | ObjectType::IndexedPartSelect
            | ObjectType::VarSelect,
        ) => {
            if bits == 1 {
                Some(ValueType::Scalar)
            } else {
                Some(ValueType::Vector)
            }
        }
        _ => None,
    };

    if let Some(value_type) = inferred_from_type {
        return value_type;
    }
    match signal.get_value(ValueType::ObjType) {
        Some(Value::Scalar(_)) => ValueType::Scalar,
        Some(Value::Vector(_)) => ValueType::Vector,
        Some(Value::Real(_)) => ValueType::Real,
        Some(Value::ShortReal(_)) => ValueType::ShortReal,
        Some(Value::String(_)) => ValueType::String,
        Some(Value::Int(_)) => ValueType::Int,
        Some(Value::ShortInt(_)) => ValueType::ShortInt,
        Some(Value::LongInt(_)) => ValueType::LongInt,
        Some(Value::Time(_)) => ValueType::Time,
        Some(Value::BinStr(_)) => ValueType::BinStr,
        Some(Value::OctStr(_)) => ValueType::OctStr,
        Some(Value::HexStr(_)) => ValueType::HexStr,
        Some(Value::DecStr(_)) => ValueType::DecStr,
        Some(Value::Strength(_)) => ValueType::Strength,
        Some(Value::ObjType(_) | Value::Suppress) | None => {
            let bits = signal.get_size().unwrap_or(1).max(1);
            if bits == 1 {
                ValueType::Scalar
            } else {
                ValueType::Vector
            }
        }
        #[cfg(feature = "verilator")]
        Some(Value::RawTwoState(_) | Value::RawFourState(_)) => ValueType::Vector,
    }
}

fn collect_signal_object(
    signal: Handle,
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    seen_signals: &mut HashSet<String>,
) {
    let object_type = signal.get_type();
    let Some(signal_name) = get_signal_full_name(&signal) else {
        return;
    };
    if !seen_signals.insert(signal_name.clone()) {
        return;
    }

    if !is_array_object_type(object_type) {
        add_signal_var(h, id_store, signal);
        return;
    }

    let signal_full_name = signal_name;

    let children: Vec<Handle> = match object_type {
        Some(ObjectType::Memory) => signal.iterator(ObjectType::MemoryWord).collect(),
        Some(ObjectType::RegArray) => signal
            .iterators(&[ObjectType::RegBit, ObjectType::Reg])
            .collect(),
        Some(ObjectType::NetArray) => signal
            .iterators(&[ObjectType::NetBit, ObjectType::Net])
            .collect(),
        Some(ObjectType::NamedEventArray) => signal.iterator(ObjectType::NamedEvent).collect(),
        Some(ObjectType::PackedArrayNet) => signal
            .iterators(&[
                ObjectType::NetBit,
                ObjectType::Net,
                ObjectType::Bit,
                ObjectType::VarSelect,
                ObjectType::PartSelect,
                ObjectType::IndexedPartSelect,
            ])
            .collect(),
        Some(ObjectType::PackedArrayVar) => signal
            .iterators(&[
                ObjectType::BitVar,
                ObjectType::RegBit,
                ObjectType::Reg,
                ObjectType::Bit,
                ObjectType::VarSelect,
                ObjectType::PartSelect,
                ObjectType::IndexedPartSelect,
            ])
            .collect(),
        _ => Vec::new(),
    };
    let mut ranges = array_ranges(&signal);
    if ranges.is_empty() {
        let inferred = infer_ranges_from_name(&signal_full_name, children.len());
        if !inferred.is_empty() {
            ranges = inferred;
        }
    }

    if children.is_empty() {
        add_signal_var(h, id_store, signal);
        return;
    }

    let name = signal_display_name(&signal).unwrap_or_else(|| UNNAMED.to_string());

    h.push(HierarchyElement::PushScope {
        name,
        scope_type: ScopeType::SvArray,
    });

    for (fallback_index, child) in children.into_iter().enumerate() {
        let child_is_array = is_array_object_type(child.get_type());

        if !child_is_array && get_signal_full_name(&child).is_none() && child.get_index().is_none()
        {
            continue;
        }

        if child_is_array {
            collect_signal_object(child, h, id_store, seen_signals);
        } else {
            let display_name = array_child_display_name(&child, fallback_index, &ranges);
            add_signal_var_named(h, id_store, child, display_name);
        }
    }

    h.push(HierarchyElement::PopScope);
}

fn collect_hierarchy_elements_from_scope(
    scope: &Handle,
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    seen_signals: &mut HashSet<String>,
) {
    let scope_name = scope.get_name().unwrap_or_else(|| UNNAMED.to_string());

    h.push(HierarchyElement::PushScope {
        name: scope_name.clone(),
        scope_type: ScopeType::Module,
    });

    let mut port_names_in_scope = HashSet::new();

    for signal in scope.iterators(&[ObjectType::Port, ObjectType::PortBit]) {
        if let Some(name) = signal_display_name(&signal) {
            port_names_in_scope.insert(name);
        }
        collect_signal_object(signal, h, id_store, seen_signals);
    }

    for signal in scope.iterators(&[
        ObjectType::Net,
        ObjectType::NetArray,
        ObjectType::Reg,
        ObjectType::Memory,
        ObjectType::RegArray,
        ObjectType::NamedEvent,
        ObjectType::NamedEventArray,
        ObjectType::PackedArrayNet,
        ObjectType::Variables,
    ]) {
        let Some(full_name) = get_signal_full_name(&signal) else {
            continue;
        };

        if !signal.is_port()
            && signal_display_name(&signal)
                .as_ref()
                .is_some_and(|name| port_names_in_scope.contains(name))
        {
            continue;
        }

        let _ = full_name;
        collect_signal_object(signal, h, id_store, seen_signals);
    }

    for child_scope in scope.iterators(&[
        ObjectType::Module,
        ObjectType::Interface,
        ObjectType::Program,
        ObjectType::Package,
        ObjectType::ClassDefn,
        ObjectType::GenScope,
        ObjectType::NamedBegin,
        ObjectType::NamedFork,
        ObjectType::InternalScope,
    ]) {
        collect_hierarchy_elements_from_scope(&child_scope, h, id_store, seen_signals);
    }

    h.push(HierarchyElement::PopScope);
}

fn collect_hierarchy() -> (Vec<HierarchyElement>, HashMap<u32, SafeHandle>) {
    let mut hierarchy = Vec::new();
    let mut id_store = IdStore::new();
    let mut seen_signals = HashSet::new();
    let root = Handle::null();

    for module in root.iterators(&[
        ObjectType::Module,
        ObjectType::Interface,
        ObjectType::Program,
        ObjectType::Package,
    ]) {
        collect_hierarchy_elements_from_scope(
            &module,
            &mut hierarchy,
            &mut id_store,
            &mut seen_signals,
        );
    }

    (hierarchy, id_store.id_to_handle)
}

fn timeout_callback(_data: &CbData) {
    vpi::printf("Timeout reached. Sending values and pausing...");
    SIMULATION_PAUSED.store(true, Ordering::SeqCst);
    {
        let mut value_changes = VALUE_CHANGES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();

        value_changes.entry(current_sim_time()).or_default();
    }
    send_changed_values(SimulationStatus::Paused);

    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    SIMULATION_PAUSED.store(false, Ordering::SeqCst);
    vpi::printf("Simulation resumed!");
}

fn send_changed_values(status: SimulationStatus) {
    let mut value_changes: Vec<TimeStep> = {
        let mut global_map = VALUE_CHANGES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();

        std::mem::take(&mut *global_map)
            .into_iter()
            .map(|(timestamp, changes)| TimeStep { timestamp, changes })
            .collect()
    };

    value_changes.sort_by_key(|ts| ts.timestamp);

    // Always emit terminal status updates even if another consumer already drained
    // buffered timesteps; this prevents losing Done/Paused state transitions.
    let should_send = !value_changes.is_empty() || status != SimulationStatus::Running;
    if should_send && let Some(connection) = CLIENT_CONNECTION.get() {
        vpi::printf(format!(
            "send_changed_values: status={status:?}, time_steps={} (done={}, paused={})",
            value_changes.len(),
            SIMULATION_DONE.load(Ordering::SeqCst),
            SIMULATION_PAUSED.load(Ordering::SeqCst)
        ));
        connection.send(SimulatorToSurferMessage::ValueChanges {
            time_steps: value_changes,
            status,
        });
    }
}

fn start_of_sim(_data: &CbData) {
    vpi::printf("Start of simulation entered");
    SIMULATION_DONE.store(false, Ordering::SeqCst);
    SIMULATION_PAUSED.store(false, Ordering::SeqCst);
    LAST_NEXTSIM_CB_TIME.store(u64::MAX, Ordering::Relaxed);

    let (hierarchy, id_to_handle) = collect_hierarchy();

    let runtime = RUNTIME_HANDLE.get().unwrap();
    let bind_address = get_bind_address_from_startup_arguments();
    runtime.spawn(message_loop(
        StaticSimData {
            hierarchy,
            id_to_handle: Arc::new(id_to_handle),
        },
        bind_address,
    ));

    let _cb = vpi::register_cb(CbReason::NextSimTime, next_time_step);

    vpi::printf("Waiting for RunSimulation message...");
    RUNTIME_HANDLE
        .get()
        .unwrap()
        .block_on(START_NOTIFY.get().unwrap().notified());
    vpi::printf("Simulation started!");
}

fn next_time_step(_data: &CbData) {
    let sim_time_for_guard = current_sim_time();
    let prev_cb_time = LAST_NEXTSIM_CB_TIME.swap(sim_time_for_guard, Ordering::Relaxed);

    let now_ms = now_millis();
    let last_time_ms = LAST_SEND_TIME_MS.load(Ordering::Relaxed);

    if now_ms - last_time_ms >= INTERVAL_MS {
        LAST_SEND_TIME_MS.store(now_ms, Ordering::Relaxed);

        if let Some(connection) = CLIENT_CONNECTION.get() {
            connection.send(SimulatorToSurferMessage::CurrentSimulationTime {
                time: current_sim_time(),
            });
        }
    }

    // Avoid self-sustaining callback loops at a fixed simulation timestamp.
    if sim_time_for_guard != prev_cb_time {
        let _cb = vpi::register_cb(CbReason::NextSimTime, next_time_step);
    }
}

fn end_of_sim(_data: &CbData) {
    SIMULATION_DONE.store(true, Ordering::SeqCst);
    SIMULATION_PAUSED.store(false, Ordering::SeqCst);
    vpi::printf("end_of_sim: transitioning to Done");
    {
        let mut value_changes = VALUE_CHANGES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();

        value_changes.entry(current_sim_time()).or_default();
    }
    send_changed_values(SimulationStatus::Done);
    vpi::printf(
        "End of simulation reached. All values sent to Surfer. Plugin will now block indefinitely.",
    );
    thread::park();
}

#[allow(clippy::too_many_lines)]
async fn handle_connection(stream: TcpStream, data: StaticSimData) {
    let (read_half, write_half) = stream.into_split();

    let mut reader = FramedRead::new(read_half, LengthDelimitedCodec::new());
    let mut writer = FramedWrite::new(write_half, LengthDelimitedCodec::new());

    // Proactively send simulator info once connected. This keeps startup robust
    // if the first inbound request frame is delayed or dropped by a proxy layer.
    let initial_info = SimulatorToSurferMessage::SimulatorInfo {
        protocol_version: PROTOCOL_VERSION,
        name: vpi::simulator_name(),
        version: vpi::simulator_version(),
    };
    match encode_simulator_to_surfer_message(&initial_info) {
        Ok(encoded) => {
            if let Err(err) = writer.send(encoded.into()).await {
                vpi::printf(format!(
                    "Failed to send initial SimulatorInfo to Surfer: {err:?}"
                ));
            }
        }
        Err(err) => {
            vpi::printf(format!(
                "Failed to encode initial SimulatorInfo for Surfer: {err:?}"
            ));
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    if CLIENT_CONNECTION.set(ClientConnection::new(tx)).is_err() {
        vpi::printf("CLIENT_CONNECTION was already set, this connection will be broken!");
    }

    loop {
        tokio::select! {
            Some(frame_result) = reader.next() => {
                let Ok(frame) = frame_result else { break };

                let msg: SurferToSimulatorMessage = match decode_surfer_to_simulator_message(&frame) {
                    Ok(msg) => msg,
                    Err(err) => {
                        vpi::printf(format!("Failed to decode message from Surfer: {err:?}"));
                        continue;
                    }
                };

                let is_shutdown = matches!(&msg, SurferToSimulatorMessage::Shutdown);
                let response: SimulatorToSurferMessage = match msg {
                    SurferToSimulatorMessage::RequestSimulatorInfo => {
                        SimulatorToSurferMessage::SimulatorInfo {
                            protocol_version: PROTOCOL_VERSION,
                            name: vpi::simulator_name(),
                            version: vpi::simulator_version(),
                        }
                    }
                    SurferToSimulatorMessage::RunSimulation { time: maybe_delay } => {
                        vpi::printf(format!("RunSimulation received: time={maybe_delay:?}"));
                        if SIMULATION_DONE.load(Ordering::SeqCst) {
                            SimulatorToSurferMessage::ValueChanges {
                                time_steps: [].to_vec(),
                                status: SimulationStatus::Done,
                            }
                        } else {
                            SIMULATION_PAUSED.store(false, Ordering::SeqCst);
                            if let Some(delay) = maybe_delay {
                                let _cb = vpi::register_cb_with_time(
                                    CbReason::AfterDelay,
                                    Time::Sim(delay),
                                    timeout_callback,
                                );
                            }
                            START_NOTIFY.get().unwrap().notify_one();
                            SimulatorToSurferMessage::Acknowledge { success: true }
                        }
                    }
                    SurferToSimulatorMessage::PauseSimulation => {
                        let _cb = vpi::register_cb_with_time(
                            CbReason::AfterDelay,
                            Time::Sim(0),
                            timeout_callback,
                        );
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                    SurferToSimulatorMessage::RequestHierarchy => {
                        let hierarchy = data.hierarchy.clone();
                        let resolution_fs = resolution_in_fs();
                        SimulatorToSurferMessage::Hierarchy {
                            hierarchy,
                            time_scale: TimeScale::from_fs(resolution_fs),
                        }
                    }
                    SurferToSimulatorMessage::TrackVariableChanges { ids } => {
                        for id in ids {
                            set_callback_on_signal(id, data.id_to_handle.as_ref());
                        }
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                    SurferToSimulatorMessage::UntrackVariableChanges { ids } => {
                        remove_callbacks(&ids);
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                    SurferToSimulatorMessage::Shutdown => {
                        vpi::printf("Shutdown received from Surfer, closing plugin connection.");
                        SimulatorToSurferMessage::Acknowledge { success: true }
                    }
                };

                let encoded = match encode_simulator_to_surfer_message(&response) {
                    Ok(encoded) => encoded,
                    Err(err) => {
                        vpi::printf(format!("Failed to serialize response to Surfer: {err:?}"));
                        continue;
                    }
                };
                if let Err(err) = writer.send(encoded.into()).await {
                    vpi::printf(format!("Failed to send response to Surfer: {err:?}"));
                    break;
                }

                if is_shutdown {
                    vpi::control(Control::Finish);
                    break;
                }
            }

            Some(plugin_msg) = rx.recv() => {
                let encoded = match encode_simulator_to_surfer_message(&plugin_msg) {
                    Ok(encoded) => encoded,
                    Err(err) => {
                        vpi::printf(format!("Failed to serialize plugin message: {err:?}"));
                        continue;
                    }
                };
                if let Err(err) = writer.send(encoded.into()).await {
                    vpi::printf(format!("Failed to send plugin message to Surfer: {err:?}"));
                    break;
                }
            }
        }
    }
}

async fn message_loop(data: StaticSimData, bind_address: String) {
    let listener = TcpListener::bind(&bind_address).await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    handle_connection(stream, data).await;
}

pub extern "C" fn vpi_plugin_startup() {
    VALUE_CHANGES.set(Mutex::new(HashMap::new())).ok();
    START_NOTIFY.set(Arc::new(Notify::new())).ok();
    CALLBACK_STORE.set(Arc::new(CallbackStore::new())).ok();

    let start_ms = now_millis();
    LAST_SEND_TIME_MS.store(start_ms, Ordering::Relaxed);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();

    Box::leak(Box::new(runtime));

    let _start_cb = vpi::register_cb(CbReason::StartOfSimulation, start_of_sim);
    let _end_cb = vpi::register_cb(CbReason::EndOfSimulation, end_of_sim);
}

fn get_bind_address_from_startup_arguments() -> String {
    let default_port: u16 = 6967;

    for argument in vpi::simulator_info().arguments {
        if let Some(port_str) = argument.strip_prefix('+') {
            if let Ok(port) = port_str.parse::<u16>() {
                let bind_address = format!("127.0.0.1:{port}");
                vpi::printf(format!("Binding simulator plugin to {bind_address}"));
                return bind_address;
            }

            vpi::printf(format!(
                "Ignoring invalid startup argument {argument:?}, expected +port"
            ));
        }
    }

    vpi::printf(format!(
        "No valid port startup argument found, using default {default_port}"
    ));
    format!("127.0.0.1:{default_port}")
}

/// Returns the simulation time resolution in femtoseconds (fs) based on the top module's timescale.
fn resolution_in_fs() -> u64 {
    let Some((_, Some(timescale))) = vpi::get_top_module_timescales().first().cloned() else {
        return 1;
    };

    // precision = -15 => 1 fs, -12 => 1000 fs, etc.
    let exp = timescale.precision + 15;
    if exp <= 0 {
        1
    } else {
        10u64.saturating_pow(exp as u32)
    }
}

/// Returns a sibling handle with the same name as the given port signal, if one exists.
fn sibling_value_handle_for_port(signal: &Handle) -> Handle {
    let Some(name) = signal.get_name() else {
        return Handle::null();
    };

    for sibling in signal.get(ObjectType::Scope).iterators(&[
        ObjectType::Net,
        ObjectType::NetArray,
        ObjectType::PackedArrayNet,
        ObjectType::Variables,
    ]) {
        if sibling.get_name().as_deref() == Some(name.as_str()) {
            return sibling;
        }
    }

    Handle::null()
}

fn callback_target_handle(signal: &Handle) -> Handle {
    if let Some(ObjectType::Port | ObjectType::PortBit) = signal.get_type() {
        let high_conn = signal.get(ObjectType::HighConn);
        if !high_conn.is_null() {
            return high_conn;
        }

        let low_conn = signal.get(ObjectType::LowConn);
        if !low_conn.is_null() {
            return low_conn;
        }

        let expr = signal.get(ObjectType::Expr);
        if !expr.is_null() {
            return expr;
        }

        let sibling = sibling_value_handle_for_port(signal);
        if !sibling.is_null() {
            return sibling;
        }
    }
    Handle::from_raw(signal.as_raw())
}

fn set_callback_on_signal(id: u32, id_to_handle: &HashMap<u32, SafeHandle>) {
    let callback_store = CALLBACK_STORE.get().unwrap();
    if callback_store.callback_from_signal_id.contains_key(&id) {
        vpi::printf(format!("Callback on {id:?} already exists"));
    } else {
        let sig_handle = id_to_handle.get(&id).unwrap().raw_handle();
        let value_handle = callback_target_handle(&sig_handle);
        let value_type = preferred_callback_value_type(&value_handle);

        // Some simulators expose array elements as select handles that are not reliable
        // callback registration targets.
        if matches!(
            value_handle.get_type(),
            Some(
                ObjectType::PartSelect
                    | ObjectType::IndexedPartSelect
                    | ObjectType::VarSelect
                    | ObjectType::RegBit
                    | ObjectType::NetBit
                    | ObjectType::Bit
            )
        ) {
            record_value_change_with_format(&value_handle, id, None, value_type);
            return;
        }

        let callback_handle = value_handle.register_value_change_cb(value_type, move |data| {
            let cb_time = data.time.as_ref().and_then(Time::to_u64);
            record_value_change_from_cbdata(data, id, cb_time, value_type);
        });
        callback_store.insert_callback(id, callback_handle);
        record_value_change_with_format(&value_handle, id, None, value_type);
    }
}

fn remove_callbacks(sig_ids: &[u32]) {
    let callback_store = CALLBACK_STORE.get().unwrap();
    let mut time_steps = VALUE_CHANGES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();

    let sig_ids: HashSet<u32> = sig_ids.iter().copied().collect();

    for &sig_id in &sig_ids {
        if let Some(callback_handle) = callback_store.remove_callback(sig_id) {
            let cb_handle = callback_handle.raw_handle();
            vpi::remove_cb(&cb_handle);
        }
    }

    for changes in time_steps.values_mut() {
        changes.retain(|change| !sig_ids.contains(&change.id));
    }

    time_steps.retain(|_timestamp, changes| !changes.is_empty());
}

fn record_value_change_with_format(
    signal_handle: &Handle,
    id: u32,
    cb_time: Option<u64>,
    value_type: ValueType,
) {
    let mut global_changes = VALUE_CHANGES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();

    if let Some(value) = vpi_signal_value_to_vcd_bytes_with_type(signal_handle, value_type) {
        let current_time = sim_time_from_callback(cb_time);

        let change = SignalChange {
            id,
            value: SignalValue::VCDValue(value),
        };

        global_changes.entry(current_time).or_default().push(change);
    }
}

fn record_value_change_from_cbdata(
    data: &CbData,
    id: u32,
    cb_time: Option<u64>,
    fallback_value_type: ValueType,
) {
    if let Some(value) = data.value.as_ref()
        && let Some(vcd_bytes) = vpi_value_to_vcd_bytes(value, &data.obj)
    {
        let mut global_changes = VALUE_CHANGES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();

        let current_time = sim_time_from_callback(cb_time);
        let change = SignalChange {
            id,
            value: SignalValue::VCDValue(vcd_bytes),
        };

        global_changes.entry(current_time).or_default().push(change);
        return;
    }

    record_value_change_with_format(&data.obj, id, cb_time, fallback_value_type);
}

fn int_to_bitvec_chars(value: u64, bits: usize) -> Vec<u8> {
    (0..bits)
        .rev()
        .map(|i| if (value >> i) & 1 == 1 { b'1' } else { b'0' })
        .collect()
}

fn vpi_signal_value_to_vcd_bytes_with_type(sig: &Handle, value_type: ValueType) -> Option<Vec<u8>> {
    let value = sig.get_value(value_type)?;
    vpi_value_to_vcd_bytes(&value, sig)
}

fn vpi_value_to_vcd_bytes(value: &Value, sig: &Handle) -> Option<Vec<u8>> {
    let declared_bits = sig.get_size().unwrap_or(1).clamp(1, 64) as usize;
    let (prefix, mut payload) = match value {
        Value::BinStr(s) if s.is_empty() => return None,
        Value::BinStr(s) => (b'b', s.clone()),
        Value::OctStr(s) => {
            // Convert octal string to binary string
            let binary_string: String = s
                .chars()
                .filter_map(|c| c.to_digit(8))
                .map(|digit| format!("{:03b}", digit))
                .collect();
            // Trim leading zeros
            let binary_string = binary_string.trim_start_matches('0').to_string();
            (b'b', binary_string)
        }
        Value::HexStr(s) => {
            // Convert hex string to binary string
            let binary_string: String = s
                .chars()
                .filter_map(|c| c.to_digit(16))
                .map(|digit| format!("{:04b}", digit))
                .collect();
            // Trim leading zeros
            let binary_string = binary_string.trim_start_matches('0').to_string();
            (b'b', binary_string)
        }
        Value::DecStr(s) => {
            // Convert decimal string to binary string
            let decimal_value = s.parse::<u64>().ok()?;
            let binary_string = int_to_bitvec_chars(decimal_value, declared_bits)
                .iter()
                .map(|b| *b as char)
                .collect();
            (b'b', binary_string)
        }
        Value::String(s) => (b's', s.clone()),
        Value::Real(f) => (b'r', f.to_string()),
        Value::ShortReal(f) => (b'r', f.to_string()),
        Value::Time(Time::Sim(t)) => (
            b'b',
            int_to_bitvec_chars(*t, declared_bits)
                .iter()
                .map(|b| *b as char)
                .collect(),
        ),
        Value::Time(Time::ScaledReal(t)) => (b'r', t.to_string()),
        Value::Int(n) => (
            b'b',
            int_to_bitvec_chars(*n as u64, declared_bits)
                .iter()
                .map(|b| *b as char)
                .collect(),
        ),
        Value::ShortInt(n) => (
            b'b',
            int_to_bitvec_chars(*n as u64, declared_bits)
                .iter()
                .map(|b| *b as char)
                .collect(),
        ),
        Value::LongInt(n) => (
            b'b',
            int_to_bitvec_chars(*n as u64, declared_bits)
                .iter()
                .map(|b| *b as char)
                .collect(),
        ),
        Value::Scalar(scalar) => return Some(vec![char::from(*scalar) as u8]),
        Value::Vector(bits) if bits.is_empty() => return None,
        Value::Vector(bits) => {
            let mut buf = Vec::with_capacity(bits.len() + 1);
            buf.push(b'b');
            for bit in bits.iter() {
                buf.push(char::from(*bit) as u8);
            }
            return Some(buf);
        }
        #[cfg(feature = "verilator")]
        Value::RawTwoState(bits) if bits.is_empty() => return None,
        #[cfg(feature = "verilator")]
        Value::RawTwoState(bits) => {
            let mut buf = Vec::with_capacity(bits.len() + 1);
            buf.push(b'b');
            for bit in bits.iter() {
                buf.push(if *bit { b'1' } else { b'0' });
            }
            return Some(buf);
        }
        #[cfg(feature = "verilator")]
        Value::RawFourState(bits) if bits.is_empty() => return None,
        #[cfg(feature = "verilator")]
        Value::RawFourState(bits) => {
            let mut buf = Vec::with_capacity(bits.len() + 1);
            buf.push(b'b');
            for bit in bits.iter() {
                buf.push(char::from(*bit) as u8);
            }
            return Some(buf);
        }
        Value::Strength(_) | Value::ObjType(_) | Value::Suppress | Value::Time(Time::Suppress) => {
            return None;
        }
    };

    let mut buf = vec![prefix];
    if matches!(value, Value::String(_))
        && let Some(stripped_prefix) = payload.strip_prefix('"')
        && let Some(stripped) = stripped_prefix.strip_suffix('"')
    {
        payload = stripped.to_string();
    }
    buf.extend_from_slice(payload.as_bytes());
    Some(buf)
}
