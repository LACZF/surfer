//! Exporting the currently displayed signals to an FST waveform file.
//!
//! Only the displayed signals (and only the hierarchy needed to reach them, not the full
//! design hierarchy) are written out.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use eyre::Result;
use fst_writer::{
    FstBodyWriter, FstFileType, FstHeaderWriter, FstInfo, FstScopeType, FstSignalId, FstSignalType,
    FstVarDirection, FstVarType, open_fst,
};
use rfd::FileHandle;
use tracing::{error, warn};
use wellen::{
    Hierarchy, ScopeType, Signal, SignalEncoding, SignalRef, Time, TimeTable, TimeTableIdx,
    VarDirection, VarRef, VarType,
};

use crate::SystemState;
use crate::async_util::perform_async_work;
use crate::channels::checked_send_many;
use crate::displayed_item::DisplayedItem;
use crate::file_dialog::FST_EXPORT_FILTER;
use crate::message::Message;
use crate::wave_container::VariableRef;

impl SystemState {
    /// Variables backing the currently displayed signals, in display order and without
    /// duplicates.
    fn displayed_variables(&self) -> Vec<VariableRef> {
        let Some(waves) = self.user.waves.as_ref() else {
            return vec![];
        };
        waves
            .items_tree
            .iter()
            .filter_map(|node| match waves.displayed_items.get(&node.item_ref) {
                Some(DisplayedItem::Variable(v)) => Some(v.variable_ref.clone()),
                _ => None,
            })
            .collect()
    }

    /// Exports the currently displayed signals to an FST file.
    ///
    /// When `path` is `None`, this opens a save dialog with a suggested filename.
    pub(crate) fn export_signals_to_fst(&mut self, path: Option<Utf8PathBuf>) {
        let variables = self.displayed_variables();
        if variables.is_empty() {
            error!("No displayed signals to export to FST");
            return;
        }
        let Some(waves) = self.user.waves.as_ref() else {
            return;
        };
        let export = match waves
            .inner
            .as_waves()
            .ok_or_else(|| eyre::eyre!("No waveform data to export"))
            .and_then(|w| w.prepare_fst_export(&variables))
        {
            Ok(export) => export,
            Err(e) => {
                error!("Failed to prepare FST export: {e:#?}");
                return;
            }
        };

        let messages = move |destination: FileHandle| async move {
            let result = match Utf8Path::from_path(destination.path()) {
                Some(path) => export.write_to_file(path),
                None => Err(eyre::eyre!(
                    "File path '{}' contains invalid UTF-8",
                    destination.path().display()
                )),
            };
            match result {
                Ok(()) => vec![],
                Err(e) => {
                    error!("Failed to export signals to FST: {e:#?}");
                    vec![Message::Error(e)]
                }
            }
        };

        if let Some(path) = path {
            let sender = self.channels.msg_sender.clone();
            perform_async_work(async move {
                checked_send_many(&sender, messages(path.into_std_path_buf().into()).await);
            });
        } else {
            self.file_dialog_save(
                "Export signals to FST",
                &FST_EXPORT_FILTER,
                Some("export.fst".to_string()),
                messages,
            );
        }
    }
}

/// Backend-agnostic, owned snapshot of the data needed to write signals out to an FST file.
pub(crate) enum FstExport {
    Wellen(FstExportData),
}

impl FstExport {
    /// Writes the previously selected variables to an FST file at `path`.
    pub(crate) fn write_to_file(&self, path: &Utf8Path) -> Result<()> {
        match self {
            FstExport::Wellen(data) => data.write_to_file(path),
        }
    }
}

/// A single node of the minimal scope tree needed to export a set of variables to FST.
///
/// Only scopes that lie on the path to an exported variable are included, and only the
/// exported variables themselves are attached, not their siblings.
#[derive(Default)]
pub(crate) struct FstExportNode {
    pub(crate) id: Option<wellen::ScopeRef>,
    pub(crate) children: BTreeMap<String, FstExportNode>,
    pub(crate) vars: Vec<VarRef>,
}

/// A self-contained snapshot of everything needed to write a set of variables (and only
/// the hierarchy needed to reach them) to an FST file.
///
/// Kept separate from `WellenContainer` so that the actual (potentially slow) file
/// writing can be handed off to a background task without holding a borrow of it.
pub(crate) struct FstExportData {
    pub(crate) hierarchy: Arc<Hierarchy>,
    pub(crate) time_table: Arc<TimeTable>,
    pub(crate) root: FstExportNode,
    /// Only the signals that are actually referenced by `root`.
    pub(crate) signals: HashMap<SignalRef, Arc<Signal>>,
    pub(crate) version: String,
    pub(crate) date: String,
    pub(crate) timescale_exponent: i8,
    pub(crate) start_time: Time,
}

impl FstExportData {
    /// Writes the previously selected variables to an FST file at `path`.
    pub(crate) fn write_to_file(&self, path: &Utf8Path) -> Result<()> {
        let h: &Hierarchy = &self.hierarchy;
        let info = FstInfo {
            start_time: self.start_time,
            timescale_exponent: self.timescale_exponent,
            version: self.version.clone(),
            date: self.date.clone(),
            file_type: FstFileType::VerilogVhdl,
        };

        let mut header = open_fst(path, &info)?;
        let mut signal_ids: HashMap<SignalRef, FstSignalId> = HashMap::new();
        write_fst_export_node(h, &mut header, &self.root, &mut signal_ids)?;
        let mut body = header.finish()?;

        // Collect the actual signal data (deduplicated) so that changes can be merged in time order.
        let signals = signal_ids
            .iter()
            .map(|(signal_ref, fst_id)| (*fst_id, Arc::clone(&self.signals[signal_ref])))
            .collect::<Vec<_>>();

        write_fst_value_changes(&mut body, &self.time_table, &signals)?;
        body.finish()?;
        Ok(())
    }
}

fn write_fst_export_node<W: std::io::Write + std::io::Seek>(
    h: &Hierarchy,
    out: &mut FstHeaderWriter<W>,
    node: &FstExportNode,
    signal_ids: &mut HashMap<SignalRef, FstSignalId>,
) -> Result<()> {
    for var_ref in &node.vars {
        write_fst_export_var(h, out, signal_ids, *var_ref)?;
    }
    for (name, child) in &node.children {
        let (component, tpe) = match child.id {
            Some(id) => {
                let scope = &h[id];
                (
                    scope.component(h).unwrap_or("").to_string(),
                    fst_scope_type(scope.scope_type()),
                )
            }
            None => (String::new(), FstScopeType::Module),
        };
        out.scope(name, &component, tpe)?;
        write_fst_export_node(h, out, child, signal_ids)?;
        out.up_scope()?;
    }
    Ok(())
}

fn write_fst_export_var<W: std::io::Write + std::io::Seek>(
    h: &Hierarchy,
    out: &mut FstHeaderWriter<W>,
    signal_ids: &mut HashMap<SignalRef, FstSignalId>,
    var_ref: VarRef,
) -> Result<()> {
    let var = &h[var_ref];
    let name = var.name(h);
    let signal_tpe = match var.signal_encoding(h) {
        SignalEncoding::String => {
            warn!(
                "Variable-length string signal '{name}' is not supported by FST export, skipping it"
            );
            return Ok(());
        }
        SignalEncoding::Real => FstSignalType::real(),
        SignalEncoding::BitVector(len) => FstSignalType::bit_vec(len),
    };
    let tpe = fst_var_type(var.var_type());
    let dir = fst_var_direction(var.direction());
    let signal_ref = var.signal_ref();
    let alias = signal_ids.get(&signal_ref).copied();
    let fst_id = out.var(name, signal_tpe, tpe, dir, alias)?;
    signal_ids.entry(signal_ref).or_insert(fst_id);
    Ok(())
}

/// Merges the value changes of `signals` in time order and writes them to `out`.
fn write_fst_value_changes<W: std::io::Write + std::io::Seek>(
    out: &mut FstBodyWriter<W>,
    time_table: &TimeTable,
    signals: &[(FstSignalId, Arc<Signal>)],
) -> Result<()> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let mut iters = signals
        .iter()
        .map(|(fst_id, signal)| (*fst_id, signal.iter_changes().peekable()))
        .collect::<Vec<_>>();

    let mut heap = BinaryHeap::new();
    for (i, (_, it)) in iters.iter_mut().enumerate() {
        if let Some((t, _)) = it.peek() {
            heap.push(Reverse((*t, i)));
        }
    }

    let mut last_time_idx: Option<TimeTableIdx> = None;
    while let Some(Reverse((time_idx, i))) = heap.pop() {
        if last_time_idx != Some(time_idx) {
            let time = time_table.get(time_idx as usize).copied().unwrap_or(0);
            out.time_change(time)?;
            last_time_idx = Some(time_idx);
        }

        let (fst_id, it) = &mut iters[i];
        let (_, value) = it.next().expect("time index was just peeked");
        match value {
            wellen::SignalValueRef::Event => out.signal_change(*fst_id, &[])?,
            wellen::SignalValueRef::BitVec(bv) => {
                out.signal_change(*fst_id, bv.bit_string().as_bytes())?;
            }
            wellen::SignalValueRef::Real(v) => out.signal_change(*fst_id, &v.to_le_bytes())?,
            wellen::SignalValueRef::String(_) => {
                // variable-length strings are not declared during export, so this should not occur
            }
        }

        if let Some((next_idx, _)) = it.peek() {
            heap.push(Reverse((*next_idx, i)));
        }
    }
    Ok(())
}

fn fst_scope_type(tpe: ScopeType) -> FstScopeType {
    match tpe {
        ScopeType::Module => FstScopeType::Module,
        ScopeType::Task => FstScopeType::Task,
        ScopeType::Function | ScopeType::VhdlFunction => FstScopeType::Function,
        ScopeType::Begin => FstScopeType::Begin,
        ScopeType::Fork => FstScopeType::Fork,
        ScopeType::Generate => FstScopeType::Generate,
        ScopeType::Struct | ScopeType::VhdlRecord => FstScopeType::Struct,
        ScopeType::Union => FstScopeType::Union,
        ScopeType::Class => FstScopeType::Class,
        ScopeType::Interface => FstScopeType::Interface,
        ScopeType::Package | ScopeType::VhdlPackage => FstScopeType::Package,
        ScopeType::Program => FstScopeType::Program,
        ScopeType::VhdlArchitecture => FstScopeType::VhdlArchitecture,
        ScopeType::VhdlProcedure => FstScopeType::VhdlProcedure,
        ScopeType::VhdlProcess => FstScopeType::VhdlProcess,
        ScopeType::VhdlBlock => FstScopeType::VhdlBlock,
        ScopeType::VhdlForGenerate => FstScopeType::VhdlForGenerate,
        ScopeType::VhdlIfGenerate => FstScopeType::VhdlIfGenerate,
        ScopeType::VhdlGenerate | ScopeType::GhwGeneric => FstScopeType::VhdlGenerate,
        ScopeType::VhdlArray | ScopeType::SvArray => FstScopeType::Struct,
        _ => FstScopeType::Module,
    }
}

fn fst_var_type(tpe: VarType) -> FstVarType {
    match tpe {
        VarType::Event | VarType::EventParameter => FstVarType::Event,
        VarType::Integer => FstVarType::Integer,
        VarType::Parameter => FstVarType::Parameter,
        VarType::Real | VarType::RealParameter => FstVarType::Real,
        VarType::Reg => FstVarType::Reg,
        VarType::Supply0 => FstVarType::Supply0,
        VarType::Supply1 => FstVarType::Supply1,
        VarType::Time => FstVarType::Time,
        VarType::Tri => FstVarType::Tri,
        VarType::TriAnd => FstVarType::TriAnd,
        VarType::TriOr => FstVarType::TriOr,
        VarType::TriReg => FstVarType::TriReg,
        VarType::Tri0 => FstVarType::Tri0,
        VarType::Tri1 => FstVarType::Tri1,
        VarType::WAnd => FstVarType::Wand,
        VarType::Wire => FstVarType::Wire,
        VarType::WOr => FstVarType::Wor,
        VarType::String => FstVarType::GenericString,
        VarType::Port => FstVarType::Port,
        VarType::SparseArray => FstVarType::SparseArray,
        VarType::RealTime => FstVarType::RealTime,
        VarType::Bit | VarType::Boolean | VarType::StdLogic | VarType::StdULogic => FstVarType::Bit,
        VarType::Logic
        | VarType::BitVector
        | VarType::StdLogicVector
        | VarType::StdULogicVector => FstVarType::Logic,
        VarType::Int => FstVarType::Int,
        VarType::ShortInt => FstVarType::ShortInt,
        VarType::LongInt => FstVarType::LongInt,
        VarType::Byte => FstVarType::Byte,
        VarType::Enum => FstVarType::Enum,
        VarType::ShortReal => FstVarType::ShortReal,
    }
}

fn fst_var_direction(dir: VarDirection) -> FstVarDirection {
    match dir {
        VarDirection::Unknown | VarDirection::Implicit => FstVarDirection::Implicit,
        VarDirection::Input => FstVarDirection::Input,
        VarDirection::Output => FstVarDirection::Output,
        VarDirection::InOut => FstVarDirection::InOut,
        VarDirection::Buffer => FstVarDirection::Buffer,
        VarDirection::Linkage => FstVarDirection::Linkage,
    }
}
