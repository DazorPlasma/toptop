//! Process telemetry and metrics inspection module.
//!
//! Handles parsing `/proc` filesystem entries to track CPU usage, resident memory,
//! threads, user ownership, disk I/O rates, and driver-reported GPU memory.

use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::Arc,
};

use crate::utils::fuzzy_match;

/// Small binary writer used for in-memory historical data archives.
///
/// The archive format is private to this process. Integer fields use varints
/// where that saves space; floating point fields are stored by their exact bit
/// pattern so archiving never rounds telemetry values.
pub(crate) struct ArchiveWriter {
    bytes: Vec<u8>,
}

impl ArchiveWriter {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.bytes.push((value as u8) | 0x80);
            value >>= 7;
        }
        self.bytes.push(value as u8);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    pub(crate) fn compact_f64(&mut self, value: f64) {
        if value.to_bits() == 0 {
            self.byte(0);
        } else {
            self.byte(1);
            self.f64(value);
        }
    }

    pub(crate) fn string(&mut self, value: &str) {
        self.varint(value.len() as u64);
        self.bytes.extend_from_slice(value.as_bytes());
    }

    pub(crate) fn string_id(&mut self, value: usize) {
        self.varint(value as u64);
    }

    pub(crate) fn finish(self) -> Arc<[u8]> {
        Arc::from(self.bytes)
    }
}

/// Reader for bytes emitted by [`ArchiveWriter`].
pub(crate) struct ArchiveReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ArchiveReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    pub(crate) fn byte(&mut self) -> u8 {
        let value = *self
            .bytes
            .get(self.cursor)
            .expect("truncated in-memory history archive");
        self.cursor += 1;
        value
    }

    pub(crate) fn varint(&mut self) -> u64 {
        let mut value = 0u64;
        for shift in (0..=63).step_by(7) {
            let byte = self.byte();
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
        }
        panic!("invalid varint in in-memory history archive");
    }

    pub(crate) fn usize(&mut self) -> usize {
        usize::try_from(self.varint()).expect("history archive length exceeds address space")
    }

    pub(crate) fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.fixed())
    }

    pub(crate) fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.fixed())
    }

    pub(crate) fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.fixed())
    }

    pub(crate) fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }

    pub(crate) fn compact_f64(&mut self) -> f64 {
        if self.byte() == 0 { 0.0 } else { self.f64() }
    }

    pub(crate) fn string(&mut self) -> String {
        let len = self.usize();
        let end = self
            .cursor
            .checked_add(len)
            .expect("history archive string length overflow");
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .expect("truncated string in in-memory history archive");
        self.cursor = end;
        String::from_utf8(bytes.to_vec()).expect("invalid UTF-8 in in-memory history archive")
    }

    pub(crate) fn string_id(&mut self, table_len: usize) -> usize {
        let id = self.usize();
        assert!(id < table_len, "invalid string id in history archive");
        id
    }

    fn fixed<const N: usize>(&mut self) -> [u8; N] {
        let end = self
            .cursor
            .checked_add(N)
            .expect("history archive offset overflow");
        let bytes: [u8; N] = self
            .bytes
            .get(self.cursor..end)
            .expect("truncated in-memory history archive")
            .try_into()
            .expect("fixed-size archive field has wrong length");
        self.cursor = end;
        bytes
    }
}

/// Deduplicates strings within a single archived sample.
pub(crate) struct ArchiveStringTable<'a> {
    ids: HashMap<&'a str, usize>,
    values: Vec<&'a str>,
    shared: Vec<Option<Arc<str>>>,
}

impl<'a> ArchiveStringTable<'a> {
    pub(crate) fn new() -> Self {
        Self {
            ids: HashMap::new(),
            values: Vec::new(),
            shared: Vec::new(),
        }
    }

    pub(crate) fn intern(&mut self, value: &'a str) -> usize {
        if let Some(&id) = self.ids.get(value) {
            return id;
        }
        let id = self.values.len();
        self.values.push(value);
        self.shared.push(None);
        self.ids.insert(value, id);
        id
    }

    pub(crate) fn intern_arc(&mut self, value: &'a Arc<str>) -> usize {
        if let Some(&id) = self.ids.get(value.as_ref()) {
            if self.shared[id].is_none() {
                self.shared[id] = Some(value.clone());
            }
            return id;
        }
        let id = self.values.len();
        self.values.push(value.as_ref());
        self.shared.push(Some(value.clone()));
        self.ids.insert(value.as_ref(), id);
        id
    }

    pub(crate) fn id(&self, value: &str) -> usize {
        *self
            .ids
            .get(value)
            .expect("string was not added to history archive table")
    }

    pub(crate) fn values(&self) -> &[&'a str] {
        &self.values
    }

    pub(crate) fn shared_values(&self) -> Vec<Arc<str>> {
        self.values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                self.shared[index]
                    .clone()
                    .unwrap_or_else(|| Arc::<str>::from(*value))
            })
            .collect()
    }
}

/// Represents detailed system telemetry and resource utilization for a running process.
#[derive(Clone, Default)]
pub struct ProcessInfo {
    /// Process Identifier (PID).
    pub pid: u32,
    /// Parent Process Identifier (PPID).
    pub ppid: u32,
    /// Short executable command name from `/proc/[pid]/comm` or `/proc/[pid]/status`.
    pub comm: Arc<str>,
    /// Process executable name or full command line invocation.
    pub name: Arc<str>,
    /// Process execution state (e.g. `R` for Running, `S` for Sleeping, `D` for Disk Sleep).
    pub state: String,
    /// Resident Set Size (RSS) from `/proc/[pid]/status` in kilobytes.
    pub rss_kb: u64,
    /// Process start time from `/proc/[pid]/stat`, used to distinguish PID reuse.
    pub starttime_ticks: u64,
    /// User mode CPU ticks from `/proc/[pid]/stat`.
    pub utime: u64,
    /// Kernel mode CPU ticks from `/proc/[pid]/stat`.
    pub stime: u64,
    /// Total cumulative bytes read from storage.
    pub read_bytes: u64,
    /// Total cumulative bytes written to storage.
    pub write_bytes: u64,
    /// Total number of active threads spawned by the process.
    pub threads: u32,
    /// Numerical User ID owning the process.
    pub uid: u32,
    /// Human-readable username resolved from `/etc/passwd`.
    pub user: Arc<str>,
    /// Real-time calculated CPU utilization percentage.
    pub cpu_percent: f64,
    /// Calculated disk read speed in bytes per second.
    pub read_speed: f64,
    /// Calculated disk write speed in bytes per second.
    pub write_speed: f64,
    /// Per-process network receive rate, when a reliable source is available.
    pub net_rx_speed: Option<f64>,
    /// Per-process network transmit rate, when a reliable source is available.
    pub net_tx_speed: Option<f64>,
    /// Group network total excludes one or more members without measurements.
    pub net_partial: bool,
    /// Busiest DRM engine utilization across the process's clients, when available.
    pub gpu_percent: Option<f64>,
    /// GPU engine state is allocated only for processes with DRM engine counters.
    pub gpu_sample: Option<Box<GpuProcessSample>>,
    /// Driver-reported process VRAM allocation in kilobytes, when available.
    pub gpu_mem_kb: Option<u64>,
    /// Proportional Set Size (PSS) from `/proc/[pid]/smaps_rollup`, when readable.
    pub pss_kb: Option<u64>,
    /// List of PIDs contained in this group if aggregated in simple view.
    pub grouped_pids: Vec<u32>,
    /// Indicates whether this row represents a collapsible group header in simple view.
    pub is_group_header: bool,
    /// Indicates whether this row is an expanded child subprocess in simple view.
    pub is_group_child: bool,
    /// Identifier of the group this row belongs to.
    pub group_name: Option<String>,
}

/// Driver counters used across polls, plus per-engine rates used by grouped rows.
#[derive(Clone, Default)]
pub struct GpuProcessSample {
    pub engine_ns: HashMap<String, u64>,
    pub engine_capacity: HashMap<String, u64>,
    pub engine_percent: HashMap<String, f64>,
    pub counter_regressed: bool,
}

/// Lossless compact representation of process rows kept in inactive snapshots.
///
/// Primitive values are serialized without precision loss, and strings are
/// deduplicated within the sample. The byte buffer is reference counted so a
/// cloned snapshot shares its archive until one copy is materialized.
#[derive(Clone, Debug, Default)]
pub struct ArchivedProcessList {
    bytes: Arc<[u8]>,
    strings: Arc<[Arc<str>]>,
}

impl ArchivedProcessList {
    pub(crate) fn encode(processes: &[ProcessInfo]) -> Self {
        let mut strings = ArchiveStringTable::new();
        for process in processes {
            strings.intern_arc(&process.comm);
            strings.intern_arc(&process.name);
            strings.intern(&process.state);
            strings.intern_arc(&process.user);
            if let Some(sample) = &process.gpu_sample {
                for key in sample
                    .engine_ns
                    .keys()
                    .chain(sample.engine_capacity.keys())
                    .chain(sample.engine_percent.keys())
                {
                    strings.intern(key);
                }
            }
            if let Some(name) = &process.group_name {
                strings.intern(name);
            }
        }

        let mut out = ArchiveWriter::with_capacity(processes.len().saturating_mul(96));
        out.byte(2); // Archive format version; shared strings are stored separately.
        out.varint(processes.len() as u64);
        for process in processes {
            out.varint(u64::from(process.pid));
            out.varint(u64::from(process.ppid));
            out.string_id(strings.id(&process.comm));
            out.string_id(strings.id(&process.name));
            out.string_id(strings.id(&process.state));
            out.varint(process.rss_kb);
            out.varint(process.starttime_ticks);
            out.varint(process.utime);
            out.varint(process.stime);
            out.varint(process.read_bytes);
            out.varint(process.write_bytes);
            out.varint(u64::from(process.threads));
            out.varint(u64::from(process.uid));
            out.string_id(strings.id(&process.user));
            out.compact_f64(process.cpu_percent);
            out.compact_f64(process.read_speed);
            out.compact_f64(process.write_speed);
            write_optional_f64(&mut out, process.net_rx_speed);
            write_optional_f64(&mut out, process.net_tx_speed);
            out.byte(u8::from(process.net_partial));
            write_optional_f64(&mut out, process.gpu_percent);
            match &process.gpu_sample {
                Some(sample) => {
                    out.byte(1);
                    write_u64_map(&mut out, &strings, &sample.engine_ns);
                    write_u64_map(&mut out, &strings, &sample.engine_capacity);
                    write_f64_map(&mut out, &strings, &sample.engine_percent);
                    out.byte(u8::from(sample.counter_regressed));
                }
                None => out.byte(0),
            }
            write_optional_u64(&mut out, process.gpu_mem_kb);
            write_optional_u64(&mut out, process.pss_kb);
            out.varint(process.grouped_pids.len() as u64);
            for &pid in &process.grouped_pids {
                out.varint(u64::from(pid));
            }
            out.byte(u8::from(process.is_group_header));
            out.byte(u8::from(process.is_group_child));
            match &process.group_name {
                Some(name) => {
                    out.byte(1);
                    out.string_id(strings.id(name));
                }
                None => out.byte(0),
            }
        }

        Self {
            bytes: out.finish(),
            strings: Arc::from(strings.shared_values()),
        }
    }

    pub(crate) fn materialize(&self) -> Vec<ProcessInfo> {
        let mut input = ArchiveReader::new(&self.bytes);
        assert_eq!(
            input.byte(),
            2,
            "unsupported process history archive version"
        );
        let strings = self.strings.as_ref();
        let process_count = input.usize();
        let mut processes = Vec::with_capacity(process_count);
        for _ in 0..process_count {
            let pid = u32::try_from(input.varint()).expect("invalid PID in history archive");
            let ppid = u32::try_from(input.varint()).expect("invalid PPID in history archive");
            let comm = strings[input.string_id(strings.len())].clone();
            let name = strings[input.string_id(strings.len())].clone();
            let state = strings[input.string_id(strings.len())].to_string();
            let rss_kb = input.varint();
            let starttime_ticks = input.varint();
            let utime = input.varint();
            let stime = input.varint();
            let read_bytes = input.varint();
            let write_bytes = input.varint();
            let threads =
                u32::try_from(input.varint()).expect("invalid thread count in history archive");
            let uid = u32::try_from(input.varint()).expect("invalid UID in history archive");
            let user = strings[input.string_id(strings.len())].clone();
            let cpu_percent = input.compact_f64();
            let read_speed = input.compact_f64();
            let write_speed = input.compact_f64();
            let net_rx_speed = read_optional_f64(&mut input);
            let net_tx_speed = read_optional_f64(&mut input);
            let net_partial = input.byte() != 0;
            let gpu_percent = read_optional_f64(&mut input);
            let gpu_sample = if input.byte() != 0 {
                Some(Box::new(GpuProcessSample {
                    engine_ns: read_u64_map(&mut input, &strings),
                    engine_capacity: read_u64_map(&mut input, &strings),
                    engine_percent: read_f64_map(&mut input, &strings),
                    counter_regressed: input.byte() != 0,
                }))
            } else {
                None
            };
            let gpu_mem_kb = read_optional_u64(&mut input);
            let pss_kb = read_optional_u64(&mut input);
            let grouped_pid_count = input.usize();
            let mut grouped_pids = Vec::with_capacity(grouped_pid_count);
            for _ in 0..grouped_pid_count {
                grouped_pids.push(
                    u32::try_from(input.varint()).expect("invalid grouped PID in history archive"),
                );
            }
            let is_group_header = input.byte() != 0;
            let is_group_child = input.byte() != 0;
            let group_name = if input.byte() != 0 {
                Some(strings[input.string_id(strings.len())].to_string())
            } else {
                None
            };

            processes.push(ProcessInfo {
                pid,
                ppid,
                comm,
                name,
                state,
                rss_kb,
                starttime_ticks,
                utime,
                stime,
                read_bytes,
                write_bytes,
                threads,
                uid,
                user,
                cpu_percent,
                read_speed,
                write_speed,
                net_rx_speed,
                net_tx_speed,
                net_partial,
                gpu_percent,
                gpu_sample,
                gpu_mem_kb,
                pss_kb,
                grouped_pids,
                is_group_header,
                is_group_child,
                group_name,
            });
        }
        processes
    }
}

fn write_optional_f64(out: &mut ArchiveWriter, value: Option<f64>) {
    if let Some(value) = value {
        out.byte(1);
        out.compact_f64(value);
    } else {
        out.byte(0);
    }
}

fn read_optional_f64(input: &mut ArchiveReader<'_>) -> Option<f64> {
    (input.byte() != 0).then(|| input.compact_f64())
}

fn write_optional_u64(out: &mut ArchiveWriter, value: Option<u64>) {
    if let Some(value) = value {
        out.byte(1);
        out.varint(value);
    } else {
        out.byte(0);
    }
}

fn read_optional_u64(input: &mut ArchiveReader<'_>) -> Option<u64> {
    (input.byte() != 0).then(|| input.varint())
}

fn write_u64_map(
    out: &mut ArchiveWriter,
    strings: &ArchiveStringTable<'_>,
    map: &HashMap<String, u64>,
) {
    out.varint(map.len() as u64);
    for (key, value) in map {
        out.string_id(strings.id(key));
        out.varint(*value);
    }
}

fn read_u64_map(input: &mut ArchiveReader<'_>, strings: &[Arc<str>]) -> HashMap<String, u64> {
    let count = input.usize();
    let mut map = HashMap::with_capacity(count);
    for _ in 0..count {
        let key = strings[input.string_id(strings.len())].to_string();
        map.insert(key, input.varint());
    }
    map
}

fn write_f64_map(
    out: &mut ArchiveWriter,
    strings: &ArchiveStringTable<'_>,
    map: &HashMap<String, f64>,
) {
    out.varint(map.len() as u64);
    for (key, value) in map {
        out.string_id(strings.id(key));
        out.compact_f64(*value);
    }
}

fn read_f64_map(input: &mut ArchiveReader<'_>, strings: &[Arc<str>]) -> HashMap<String, f64> {
    let count = input.usize();
    let mut map = HashMap::with_capacity(count);
    for _ in 0..count {
        let key = strings[input.string_id(strings.len())].to_string();
        map.insert(key, input.compact_f64());
    }
    map
}

/// Target process details for termination/kill validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessTarget {
    /// Process Identifier (PID).
    pub pid: u32,
    /// Expected command name.
    pub comm: String,
    /// Expected process name.
    pub name: String,
}

/// Stores pending process termination confirmation modal state.
#[derive(Clone, Debug)]
pub struct ProcessKillConfirmation {
    /// List of target processes to validate and terminate upon confirmation.
    pub targets: Vec<ProcessTarget>,
    /// List of PIDs to send signal to upon confirmation.
    pub pids: Vec<u32>,
    /// Name or summary of the target process/group.
    pub process_name: String,
    /// Signal to dispatch (`Signal::TERM` for SIGTERM, `Signal::KILL` for SIGKILL).
    pub signal: rustix::process::Signal,
    /// Indicates whether this is a forceful SIGKILL (`true`) or graceful SIGTERM (`false`).
    pub is_kill: bool,
}

/// Stores process validation error popup state.
#[derive(Clone, Debug)]
pub struct ProcessErrorPopup {
    /// Title of the error popup.
    pub title: String,
    /// Message lines explaining the validation error.
    pub message_lines: Vec<String>,
}

/// Detailed diagnostic and telemetry inspection for an individual process.
#[derive(Clone, Default)]
pub struct ProcessDetailInfo {
    /// Basic process telemetry snapshot.
    pub info: ProcessInfo,
    /// Whether this detail page is viewing a saved snapshot rather than the latest live view.
    pub snapshot_is_historical: bool,
    /// Whether the current PID has been reused by a different process since this snapshot.
    pub pid_reused: bool,
    /// Whether a matching live `/proc/[pid]` entry was available for extra detail.
    pub live_metadata_available: bool,
    /// Parent process command name.
    pub parent_comm: String,
    /// Target binary executable path from `/proc/[pid]/exe`.
    pub exe_path: String,
    /// Current working directory from `/proc/[pid]/cwd`.
    pub cwd_path: String,
    /// Full un-truncated command line invocation.
    pub cmdline: String,
    /// Virtual memory peak size in kilobytes (VmPeak).
    pub vm_peak_kb: u64,
    /// Virtual memory current size in kilobytes (VmSize).
    pub vm_size_kb: u64,
    /// Peak resident set size in kilobytes (VmHWM).
    pub vm_hwm_kb: u64,
    /// Swap consumption in kilobytes (VmSwap).
    pub vm_swap_kb: u64,
    /// Data segment size in kilobytes (VmData).
    pub vm_data_kb: u64,
    /// Stack size in kilobytes (VmStk).
    pub vm_stk_kb: u64,
    /// Executable code segment size in kilobytes (VmExe).
    pub vm_exe_kb: u64,
    /// Shared library size in kilobytes (VmLib).
    pub vm_lib_kb: u64,
    /// Number of open file descriptors in `/proc/[pid]/fd`.
    pub open_fds: usize,
    /// Priority level from `/proc/[pid]/stat`.
    pub priority: i64,
    /// Nice level from `/proc/[pid]/stat`.
    pub nice: i64,
    /// Voluntary context switches from `/proc/[pid]/status`.
    pub voluntary_ctxt_switches: u64,
    /// Involuntary context switches from `/proc/[pid]/status`.
    pub nonvoluntary_ctxt_switches: u64,
}

/// Identifies the column currently selected for process sorting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSortColumn {
    /// Sort by Process Identifier (PID).
    Pid,
    /// Sort by username.
    User,
    /// Sort alphabetically by process name / command line.
    Name,
    /// Sort by process execution state.
    State,
    /// Sort by number of spawned threads.
    Threads,
    /// Sort by CPU usage percentage.
    Cpu,
    /// Sort by resident set size (RSS).
    Mem,
    /// Sort by GPU core utilization percentage.
    Gpu,
    /// Sort by GPU VRAM allocation.
    GpuMem,
    /// Sort by disk I/O throughput speed.
    Io,
    /// Sort by network throughput speed.
    Net,
}

/// Default sortable columns visible in standard process table view.
pub const NORMAL_SORT_COLUMNS: [ProcessSortColumn; 8] = [
    ProcessSortColumn::Pid,
    ProcessSortColumn::Name,
    ProcessSortColumn::Cpu,
    ProcessSortColumn::Mem,
    ProcessSortColumn::Gpu,
    ProcessSortColumn::GpuMem,
    ProcessSortColumn::Io,
    ProcessSortColumn::Net,
];

/// Full list of sortable columns available in advanced process view mode.
pub const ADVANCED_SORT_COLUMNS: [ProcessSortColumn; 11] = [
    ProcessSortColumn::Pid,
    ProcessSortColumn::User,
    ProcessSortColumn::Name,
    ProcessSortColumn::State,
    ProcessSortColumn::Threads,
    ProcessSortColumn::Cpu,
    ProcessSortColumn::Mem,
    ProcessSortColumn::Gpu,
    ProcessSortColumn::GpuMem,
    ProcessSortColumn::Io,
    ProcessSortColumn::Net,
];

fn compare_optional<T>(
    a: &Option<T>,
    b: &Option<T>,
    ascending: bool,
    compare: impl FnOnce(&T, &T) -> std::cmp::Ordering,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (a, b) {
        (Some(a), Some(b)) => {
            let ordering = compare(a, b);
            if ascending {
                ordering
            } else {
                ordering.reverse()
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn optional_rate_max(rx: Option<f64>, tx: Option<f64>) -> Option<f64> {
    match (rx, tx) {
        (Some(rx), Some(tx)) => Some(rx.max(tx)),
        (Some(rate), None) | (None, Some(rate)) => Some(rate),
        (None, None) => None,
    }
}

fn sum_optional_u64(values: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    values.into_iter().try_fold(0u64, |total, value| {
        value.map(|value| total.saturating_add(value))
    })
}

/// Sorts an in-memory slice of `ProcessInfo` entries in place according to the specified column and direction.
///
/// # Arguments
/// * `processes` - Mutable slice of processes to sort.
/// * `sort_col` - The target column to sort by.
/// * `ascending` - `true` for ascending order, `false` for descending.
pub fn sort_processes(processes: &mut [ProcessInfo], sort_col: ProcessSortColumn, ascending: bool) {
    processes.sort_by(|a, b| {
        let cmp = match sort_col {
            ProcessSortColumn::Pid => a.pid.cmp(&b.pid),
            ProcessSortColumn::User => a.user.to_lowercase().cmp(&b.user.to_lowercase()),
            ProcessSortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            ProcessSortColumn::State => a.state.cmp(&b.state),
            ProcessSortColumn::Threads => a.threads.cmp(&b.threads),
            ProcessSortColumn::Cpu => a
                .cpu_percent
                .partial_cmp(&b.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal),
            ProcessSortColumn::Mem => a.rss_kb.cmp(&b.rss_kb),
            ProcessSortColumn::Gpu => {
                compare_optional(&a.gpu_percent, &b.gpu_percent, ascending, |a, b| {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                })
            }
            ProcessSortColumn::GpuMem => {
                compare_optional(&a.gpu_mem_kb, &b.gpu_mem_kb, ascending, u64::cmp)
            }
            ProcessSortColumn::Io => {
                let a_io = a.read_speed.max(a.write_speed);
                let b_io = b.read_speed.max(b.write_speed);
                a_io.partial_cmp(&b_io).unwrap_or(std::cmp::Ordering::Equal)
            }
            ProcessSortColumn::Net => compare_optional(
                &optional_rate_max(a.net_rx_speed, a.net_tx_speed),
                &optional_rate_max(b.net_rx_speed, b.net_tx_speed),
                ascending,
                |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
            ),
        };
        if matches!(
            sort_col,
            ProcessSortColumn::Gpu | ProcessSortColumn::GpuMem | ProcessSortColumn::Net
        ) {
            cmp
        } else if ascending {
            cmp
        } else {
            cmp.reverse()
        }
    });
}

/// Parses a DRM memory size string with units (e.g. "6279292 KiB", "2 MiB", "6.8 GiB", "0") into KiB.
pub fn parse_drm_memory_kb(line: &str) -> u64 {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return 0;
    }
    let val = match parts[1].parse::<f64>() {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let unit = parts.get(2).map(|s| s.to_lowercase()).unwrap_or_default();
    if unit.starts_with("gib") || unit.starts_with("gb") {
        (val * 1024.0 * 1024.0).round() as u64
    } else if unit.starts_with("mib") || unit.starts_with("mb") {
        (val * 1024.0).round() as u64
    } else if unit.starts_with("kib") || unit.starts_with("kb") {
        val.round() as u64
    } else if unit.starts_with('b') || unit.is_empty() {
        (val / 1024.0).round() as u64
    } else {
        val.round() as u64
    }
}

/// Reads driver-reported GPU memory and engine counters from `/proc/[pid]/fdinfo`.
///
/// GTT/system-memory counters are deliberately excluded: they are not VRAM, and the
/// process table compares this value with the device's VRAM capacity.
fn read_process_gpu_stats(pid: u32) -> (Option<u64>, HashMap<String, u64>, HashMap<String, u64>) {
    type ClientStats = (Option<u64>, HashMap<String, u64>, HashMap<String, u64>);
    let mut client_map: HashMap<(String, String), ClientStats> = HashMap::new();

    if let Ok(fd_entries) = fs::read_dir(format!("/proc/{}/fdinfo", pid)) {
        for fd_entry in fd_entries.filter_map(Result::ok) {
            if let Ok(fd_content) = fs::read_to_string(fd_entry.path()) {
                let mut is_drm = false;
                let mut pdev = None;
                let mut client_id = None;
                let mut vram_kb = None;
                let mut engine_ns = HashMap::new();
                let mut engine_capacity = HashMap::new();

                for line in fd_content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("drm-driver:") {
                        is_drm = true;
                    } else if trimmed.starts_with("drm-pdev:") {
                        is_drm = true;
                        pdev = trimmed
                            .split_once(':')
                            .map(|(_, value)| value.trim().to_string())
                            .filter(|value| !value.is_empty());
                    } else if trimmed.starts_with("drm-client-id:") {
                        is_drm = true;
                        if let Some(id_str) = trimmed.split_whitespace().nth(1)
                            && let Ok(id) = id_str.parse::<u64>()
                        {
                            client_id = Some(id);
                        }
                    } else if trimmed.starts_with("drm-resident-vram:")
                        || trimmed.starts_with("drm-resident-local:")
                        || trimmed.starts_with("drm-memory-vram:")
                        || trimmed.starts_with("drm-memory-local:")
                    {
                        is_drm = true;
                        let parsed = parse_drm_memory_kb(trimmed);
                        vram_kb = Some(vram_kb.unwrap_or(0).max(parsed));
                    } else if let Some((key, value)) = trimmed.split_once(':')
                        && let Some(engine) = key.strip_prefix("drm-engine-capacity-")
                        && let Some(capacity) = value.trim().parse::<u64>().ok()
                        && capacity > 0
                    {
                        is_drm = true;
                        engine_capacity.insert(engine.to_string(), capacity);
                    } else if let Some((key, value)) = trimmed.split_once(':')
                        && let Some(engine) = key.strip_prefix("drm-engine-")
                        && !engine.starts_with("capacity-")
                        && value
                            .split_whitespace()
                            .nth(1)
                            .is_some_and(|unit| unit.eq_ignore_ascii_case("ns"))
                        && let Some(ns) = value
                            .split_whitespace()
                            .next()
                            .and_then(|value| value.parse::<u64>().ok())
                    {
                        is_drm = true;
                        engine_ns
                            .entry(engine.to_string())
                            .and_modify(|current: &mut u64| *current = (*current).max(ns))
                            .or_insert(ns);
                    }
                }

                if is_drm {
                    let device_key = pdev.unwrap_or_else(|| "unknown-device".to_string());
                    let client_key =
                        client_id
                            .map(|id| format!("client:{id}"))
                            .unwrap_or_else(|| {
                                format!("fd:{}", fd_entry.file_name().to_string_lossy())
                            });
                    let entry = client_map
                        .entry((device_key, client_key))
                        .or_insert_with(|| (None, HashMap::new(), HashMap::new()));
                    if let Some(vram_kb) = vram_kb {
                        entry.0 = Some(entry.0.unwrap_or(0).max(vram_kb));
                    }
                    for (engine, ns) in engine_ns {
                        entry
                            .1
                            .entry(engine)
                            .and_modify(|current| *current = (*current).max(ns))
                            .or_insert(ns);
                    }
                    for (engine, capacity) in engine_capacity {
                        entry.2.insert(engine, capacity);
                    }
                }
            }
        }
    }

    let mut total_vram_kb = 0u64;
    let mut has_vram_usage = false;
    let mut total_engine_ns = HashMap::new();
    let mut total_engine_capacity = HashMap::new();
    for ((device, _client), (vram_kb, engines, capacity)) in client_map {
        if let Some(vram_kb) = vram_kb {
            total_vram_kb = total_vram_kb.saturating_add(vram_kb);
            has_vram_usage = true;
        }
        for (engine, ns) in engines {
            let key = format!("{device}/{engine}");
            total_engine_ns
                .entry(key.clone())
                .and_modify(|total: &mut u64| *total = total.saturating_add(ns))
                .or_insert(ns);
            let count = capacity.get(&engine).copied().unwrap_or(1);
            total_engine_capacity
                .entry(key)
                .and_modify(|current: &mut u64| *current = (*current).max(count))
                .or_insert(count);
        }
    }

    (
        has_vram_usage.then_some(total_vram_kb),
        total_engine_ns,
        total_engine_capacity,
    )
}

/// Queries NVIDIA proprietary driver GPU memory allocations per PID using nvidia-smi if available.
pub fn read_nvidia_gpu_mem() -> HashMap<u32, u64> {
    let mut map = HashMap::new();
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_gpu_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2
                && let Ok(pid) = parts[0].trim().parse::<u32>()
                && let Ok(mib) = parts[1].trim().parse::<u64>()
            {
                *map.entry(pid).or_insert(0) += mib * 1024;
            }
        }
    }
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-graphics-apps=pid,used_gpu_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2
                && let Ok(pid) = parts[0].trim().parse::<u32>()
                && let Ok(mib) = parts[1].trim().parse::<u64>()
            {
                map.entry(pid).or_insert(mib * 1024);
            }
        }
    }
    map
}

/// Reads and parses all running processes from the `/proc` virtual filesystem.
///
/// Computes delta rates for CPU percentage and disk read/write throughput by
/// comparing current counters against the previous process sample. Linux does
/// not expose a reliable per-process network counter through these sources, so
/// network rates remain unavailable instead of being inferred from disk I/O.
///
/// # Arguments
/// * `prev_procs` - Cache of previous process states keyed by PID.
/// * `users` - Lookup map translating UID to username.
/// * `dt` - Elapsed time delta in seconds since the previous tick.
///
/// # Returns
/// A `Vec<ProcessInfo>` containing active process metrics.
pub fn read_processes(
    prev_procs: &mut HashMap<u32, ProcessInfo>,
    users: &HashMap<u32, String>,
    dt: f64,
) -> Vec<ProcessInfo> {
    let mut procs = Vec::new();
    let mut new_prev_procs = HashMap::new();
    let nvidia_gpu_mem = read_nvidia_gpu_mem();

    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.filter_map(Result::ok) {
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();
            if let Ok(pid) = file_name_str.parse::<u32>() {
                let mut p = ProcessInfo {
                    pid,
                    ..Default::default()
                };

                if let Ok(status) = fs::read_to_string(format!("/proc/{}/status", pid)) {
                    for line in status.lines() {
                        if line.starts_with("Name:") {
                            p.comm = Arc::from(line.trim_start_matches("Name:").trim());
                            p.name = p.comm.clone();
                        } else if line.starts_with("State:") {
                            p.state = line.trim_start_matches("State:").trim().to_string();
                        } else if line.starts_with("VmRSS:") {
                            let mut parts = line.split_whitespace();
                            parts.next();
                            p.rss_kb = parts.next().unwrap_or("0").parse().unwrap_or(0);
                        } else if line.starts_with("Uid:") {
                            let mut parts = line.split_whitespace();
                            parts.next();
                            p.uid = parts.next().unwrap_or("0").parse().unwrap_or(0);
                        }
                    }
                }

                if let Ok(cmdline_bytes) = fs::read(format!("/proc/{}/cmdline", pid))
                    && !cmdline_bytes.is_empty()
                {
                    let cmd = String::from_utf8_lossy(&cmdline_bytes)
                        .replace('\0', " ")
                        .trim()
                        .to_string();
                    if !cmd.is_empty() {
                        p.name = cmd.into();
                    }
                }

                p.user = users
                    .get(&p.uid)
                    .cloned()
                    .unwrap_or_else(|| p.uid.to_string())
                    .into();

                if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid))
                    && let Some(rparen) = stat.rfind(')')
                {
                    let rest = &stat[rparen + 1..];
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.len() >= 20 {
                        p.ppid = parts[1].parse().unwrap_or(0);
                        p.utime = parts[11].parse().unwrap_or(0);
                        p.stime = parts[12].parse().unwrap_or(0);
                        p.threads = parts[17].parse().unwrap_or(0);
                        p.starttime_ticks = parts[19].parse().unwrap_or(0);
                    }
                }

                if let Ok(io) = fs::read_to_string(format!("/proc/{}/io", pid)) {
                    for line in io.lines() {
                        if line.starts_with("read_bytes:") {
                            p.read_bytes = line
                                .trim_start_matches("read_bytes:")
                                .trim()
                                .parse()
                                .unwrap_or(0);
                        } else if line.starts_with("write_bytes:") {
                            p.write_bytes = line
                                .trim_start_matches("write_bytes:")
                                .trim()
                                .parse()
                                .unwrap_or(0);
                        }
                    }
                }

                if let Ok(smaps) = fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)) {
                    for line in smaps.lines() {
                        if line.starts_with("Pss:") {
                            let mut parts = line.split_whitespace();
                            parts.next();
                            p.pss_kb = parts.next().and_then(|value| value.parse().ok());
                        }
                    }
                }

                let (drm_gpu_kb, gpu_engine_ns, gpu_engine_capacity) = read_process_gpu_stats(pid);
                let nv_gpu_kb = nvidia_gpu_mem.get(&pid).copied();
                p.gpu_mem_kb = drm_gpu_kb.or(nv_gpu_kb);
                if !gpu_engine_ns.is_empty() {
                    p.gpu_sample = Some(Box::new(GpuProcessSample {
                        engine_ns: gpu_engine_ns,
                        engine_capacity: gpu_engine_capacity,
                        ..Default::default()
                    }));
                }

                if let Some(prev) = prev_procs.get(&pid)
                    && dt > 0.0
                    && p.starttime_ticks != 0
                    && p.starttime_ticks == prev.starttime_ticks
                {
                    let delta_ticks = (p.utime + p.stime).saturating_sub(prev.utime + prev.stime);
                    p.cpu_percent = (delta_ticks as f64) / dt;
                    p.read_speed = p.read_bytes.saturating_sub(prev.read_bytes) as f64 / dt;
                    p.write_speed = p.write_bytes.saturating_sub(prev.write_bytes) as f64 / dt;

                    if let (Some(sample), Some(prev_sample)) =
                        (p.gpu_sample.as_mut(), prev.gpu_sample.as_deref())
                    {
                        let engine_set_unchanged = sample.engine_ns.len()
                            == prev_sample.engine_ns.len()
                            && sample
                                .engine_ns
                                .keys()
                                .all(|engine| prev_sample.engine_ns.contains_key(engine));
                        if engine_set_unchanged {
                            let regressed = sample
                                .engine_ns
                                .iter()
                                .any(|(engine, now)| now < &prev_sample.engine_ns[engine]);
                            let recovered_after_dip = sample
                                .engine_ns
                                .iter()
                                .any(|(engine, now)| now > &prev_sample.engine_ns[engine]);

                            if regressed {
                                sample.counter_regressed = true;
                                for (engine, now) in &mut sample.engine_ns {
                                    *now = (*now).max(prev_sample.engine_ns[engine]);
                                }
                            } else if prev_sample.counter_regressed && !recovered_after_dip {
                                p.gpu_percent = None;
                                sample.counter_regressed = true;
                            } else {
                                sample.engine_percent = sample
                                    .engine_ns
                                    .iter()
                                    .map(|(engine, now)| {
                                        let delta = now - prev_sample.engine_ns[engine];
                                        let capacity = sample
                                            .engine_capacity
                                            .get(engine)
                                            .copied()
                                            .unwrap_or(1);
                                        let percent = delta as f64
                                            / (dt * 1_000_000_000.0 * capacity as f64)
                                            * 100.0;
                                        (engine.clone(), percent)
                                    })
                                    .collect();
                                p.gpu_percent = sample
                                    .engine_percent
                                    .values()
                                    .copied()
                                    .reduce(f64::max)
                                    .map(|value| value.clamp(0.0, 100.0));
                                sample.counter_regressed = false;
                            }
                        }
                    }
                }

                if p.rss_kb > 0 || !p.name.is_empty() {
                    if let Some(prev) = prev_procs.get(&pid)
                        && p.starttime_ticks != 0
                        && p.starttime_ticks == prev.starttime_ticks
                    {
                        if p.comm == prev.comm {
                            p.comm = prev.comm.clone();
                        }
                        if p.name == prev.name {
                            p.name = prev.name.clone();
                        }
                        if p.user == prev.user {
                            p.user = prev.user.clone();
                        }
                    }
                    new_prev_procs.insert(pid, p.clone());
                    procs.push(p);
                }
            }
        }
    }
    *prev_procs = new_prev_procs;
    procs
}

/// Reads deep diagnostic telemetry for a specific process from `/proc/[pid]`.
///
/// # Arguments
/// * `proc_info` - Baseline process info snapshot.
/// * `proc_map` - Map of currently known processes for parent lookups.
/// * `snapshot_is_historical` - Whether to keep the detail strictly snapshot-only.
///
/// # Returns
/// A populated `ProcessDetailInfo` structure.
pub fn read_process_detail(
    proc_info: &ProcessInfo,
    proc_map: &HashMap<u32, &ProcessInfo>,
    snapshot_is_historical: bool,
) -> ProcessDetailInfo {
    let pid = proc_info.pid;
    let mut detail = ProcessDetailInfo {
        info: proc_info.clone(),
        snapshot_is_historical,
        cmdline: proc_info.name.to_string(),
        ..Default::default()
    };

    if let Some(parent) = proc_map.get(&proc_info.ppid) {
        detail.parent_comm = parent.comm.to_string();
    }
    if snapshot_is_historical {
        detail.cmdline = proc_info.name.to_string();
        return detail;
    }

    let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid)) else {
        return detail;
    };
    let Some(rparen) = stat.rfind(')') else {
        return detail;
    };
    let parts: Vec<&str> = stat[rparen + 1..].split_whitespace().collect();
    let Some(live_starttime) = parts.get(19).and_then(|value| value.parse::<u64>().ok()) else {
        return detail;
    };
    if proc_info.starttime_ticks > 0 && proc_info.starttime_ticks != live_starttime {
        detail.pid_reused = true;
        return detail;
    }
    detail.live_metadata_available = true;

    if detail.parent_comm.is_empty()
        && let Ok(comm) = fs::read_to_string(format!("/proc/{}/comm", proc_info.ppid))
    {
        detail.parent_comm = comm.trim().to_string();
    }

    if let Ok(exe) = fs::read_link(format!("/proc/{}/exe", pid)) {
        detail.exe_path = exe.to_string_lossy().to_string();
    }

    if let Ok(cwd) = fs::read_link(format!("/proc/{}/cwd", pid)) {
        detail.cwd_path = cwd.to_string_lossy().to_string();
    }

    if let Ok(cmd_bytes) = fs::read(format!("/proc/{}/cmdline", pid)) {
        detail.cmdline = String::from_utf8_lossy(&cmd_bytes)
            .replace('\0', " ")
            .trim()
            .to_string();
    }
    if detail.cmdline.is_empty() {
        detail.cmdline = proc_info.name.to_string();
    }

    if let Ok(entries) = fs::read_dir(format!("/proc/{}/fd", pid)) {
        detail.open_fds = entries.filter_map(Result::ok).count();
    }

    detail.priority = parts[15].parse().unwrap_or(0);
    detail.nice = parts[16].parse().unwrap_or(0);

    if let Ok(status) = fs::read_to_string(format!("/proc/{}/status", pid)) {
        for line in status.lines() {
            let parse_val = |l: &str| -> u64 {
                l.split_whitespace()
                    .nth(1)
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0)
            };
            if line.starts_with("VmPeak:") {
                detail.vm_peak_kb = parse_val(line);
            } else if line.starts_with("VmSize:") {
                detail.vm_size_kb = parse_val(line);
            } else if line.starts_with("VmHWM:") {
                detail.vm_hwm_kb = parse_val(line);
            } else if line.starts_with("VmSwap:") {
                detail.vm_swap_kb = parse_val(line);
            } else if line.starts_with("VmData:") {
                detail.vm_data_kb = parse_val(line);
            } else if line.starts_with("VmStk:") {
                detail.vm_stk_kb = parse_val(line);
            } else if line.starts_with("VmExe:") {
                detail.vm_exe_kb = parse_val(line);
            } else if line.starts_with("VmLib:") {
                detail.vm_lib_kb = parse_val(line);
            } else if line.starts_with("voluntary_ctxt_switches:") {
                detail.voluntary_ctxt_switches = parse_val(line);
            } else if line.starts_with("nonvoluntary_ctxt_switches:") {
                detail.nonvoluntary_ctxt_switches = parse_val(line);
            }
        }
    }

    detail
}

/// Strips directory paths and packaging wrapper artifacts from an executable filename.
///
/// # Arguments
/// * `path` - Raw executable token or path.
///
/// # Returns
/// Cleaned base executable name.
pub fn strip_binary_path(path: &str) -> &str {
    let mut name = path;
    if let Some(pos) = name.rfind('/') {
        name = &name[pos + 1..];
    }
    // Strip leading dot from wrappers like .pavucontrol-wrapped
    name = name.trim_start_matches('.');
    // Strip wrapper suffixes like -wrapped or .wrapped
    if let Some(stripped) = name.strip_suffix("-wrapped") {
        name = stripped;
    } else if let Some(stripped) = name.strip_suffix(".wrapped") {
        name = stripped;
    }
    name
}

/// Formats and simplifies raw command lines for human-readable display in simple view.
/// Resolves interpreters, runtime scripts, and strips directory paths from arguments.
///
/// # Arguments
/// * `raw_cmd` - Full command line invocation string.
///
/// # Returns
/// Simplified process command string.
pub fn clean_process_command(raw_cmd: &str) -> String {
    let trimmed = raw_cmd.trim();
    if trimmed.is_empty() || trimmed.starts_with('[') {
        return trimmed.to_string();
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return String::new();
    }

    let bin = strip_binary_path(tokens[0]);

    // Check for runtime/interpreter
    let is_python = bin.starts_with("python") || bin.starts_with("pypy");
    let is_node = bin == "node" || bin == "nodejs" || bin == "bun" || bin == "deno";
    let is_ruby = bin == "ruby";
    let is_perl = bin == "perl";
    let is_shell = bin == "bash" || bin == "sh" || bin == "zsh" || bin == "fish";
    let is_electron = bin == "electron" || bin == "electron-unwrapped";
    let is_java = bin == "java";

    if is_electron {
        // Look for application asar or js
        for token in &tokens[1..] {
            if token.ends_with(".asar") || token.contains("/app.asar") {
                let asar_file = strip_binary_path(token);
                let app = if let Some(pos) = token.find("/share/") {
                    let rest = &token[pos + 7..];
                    if let Some(slash) = rest.find('/') {
                        &rest[..slash]
                    } else {
                        asar_file
                    }
                } else if asar_file == "app.asar" {
                    let dir = token.trim_end_matches("/app.asar");
                    strip_binary_path(dir)
                } else {
                    asar_file
                };
                let rest_args = tokens[1..]
                    .iter()
                    .filter(|t| {
                        !t.ends_with(".asar")
                            && !t.contains("/app.asar")
                            && !t.starts_with("--ozone")
                            && !t.starts_with("--enable-features")
                    })
                    .copied()
                    .collect::<Vec<&str>>()
                    .join(" ");
                if rest_args.is_empty() {
                    return format!("{app} ({bin})");
                }
                return format!("{app} ({bin}) {rest_args}");
            }
        }
    }

    if is_python || is_node || is_ruby || is_perl || is_shell {
        // Find the target script (first argument not starting with '-')
        if let Some(script_idx) = tokens[1..].iter().position(|t| !t.starts_with('-')) {
            let actual_idx = 1 + script_idx;
            let script_name = strip_binary_path(tokens[actual_idx]);
            let mut result = format!("{script_name} ({bin})");
            let mut remaining = Vec::new();
            for (i, &tok) in tokens.iter().enumerate() {
                if i != 0 && i != actual_idx {
                    remaining.push(tok);
                }
            }
            if !remaining.is_empty() {
                result.push(' ');
                result.push_str(&remaining.join(" "));
            }
            return result;
        }
    }

    if is_java
        && let Some(jar_idx) = tokens.iter().position(|&t| t == "-jar")
        && let Some(jar_target) = tokens.get(jar_idx + 1)
    {
        let jar_name = strip_binary_path(jar_target);
        let mut remaining = Vec::new();
        for (i, &tok) in tokens.iter().enumerate() {
            if i != 0 && i != jar_idx && i != jar_idx + 1 {
                remaining.push(tok);
            }
        }
        let mut result = format!("{jar_name} (java)");
        if !remaining.is_empty() {
            result.push(' ');
            result.push_str(&remaining.join(" "));
        }
        return result;
    }

    // Default: replace binary token with base name
    let mut res = vec![bin];
    res.extend_from_slice(&tokens[1..]);
    res.join(" ")
}

/// Identifies whether a process belongs to a known multi-process application suite.
///
/// # Arguments
/// * `name` - Process executable name or command line.
///
/// # Returns
/// Optional static group name string.
pub fn identify_process_group(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let bin = tokens.first().map(|s| strip_binary_path(s)).unwrap_or("");

    if lower.contains("librewolf")
        || name.starts_with("Isolated Web Co")
        || name.starts_with("Isolated Servic")
        || name.starts_with("Privileged Cont")
        || name.starts_with("Socket Process")
        || name.starts_with("RDD Process")
    {
        if lower.contains("firefox") {
            return Some("Firefox");
        }
        return Some("LibreWolf");
    }
    if lower.contains("firefox") || name.starts_with("GeckoMain") {
        return Some("Firefox");
    }
    if lower.contains("google-chrome")
        || lower.contains("chromium")
        || bin == "chrome"
        || bin == "chromium"
        || lower.contains("chrome-sandbox")
    {
        return Some("Chrome");
    }
    if lower.contains("brave-browser") || bin == "brave" || lower.contains("/brave") {
        return Some("Brave");
    }
    if lower.contains("msedge") || bin == "msedge" {
        return Some("Edge");
    }
    if lower.contains("vesktop") {
        return Some("Vesktop");
    }
    if lower.contains("discord") {
        return Some("Discord");
    }
    if lower.contains("spotify") {
        return Some("Spotify");
    }
    if lower.contains("slack") {
        return Some("Slack");
    }
    if lower.contains("obsidian") {
        return Some("Obsidian");
    }
    if lower.contains("steam") || lower.contains("steamwebhelper") {
        return Some("Steam");
    }
    if lower.contains("kitty")
        || bin == "kitten"
        || lower.contains("/kitten")
        || bin == "foot"
        || bin == "footclient"
        || lower.starts_with("foot ")
        || lower == "foot"
        || lower.contains("/foot ")
        || lower.contains("/footclient")
        || lower.contains(".foot-wrapped")
        || lower.contains("foot-server")
        || lower.contains("alacritty")
        || lower.contains("wezterm")
        || lower.contains("ghostty")
    {
        return Some("Terminals");
    }
    if lower.contains("vscode")
        || lower.contains("vscodium")
        || lower.contains("codium")
        || bin == "code"
        || bin == "code-oss"
        || lower.contains("/.vscode/")
        || lower.contains("/.config/code/")
    {
        return Some("VS Code");
    }
    if bin == "rust-analyzer" || bin == "rust_analyzer" {
        return Some("Rust Analyzer");
    }
    if bin == "gvfsd" || bin.starts_with("gvfsd-") || bin.starts_with("gvfs-") {
        return Some("Gnome Virtual FileSystem");
    }
    if bin == "dockerd"
        || bin == "containerd"
        || bin.starts_with("containerd-shim")
        || bin == "docker-proxy"
        || bin == "docker"
    {
        return Some("Docker");
    }
    if bin == "syncthing" || bin.starts_with("syncthing-") {
        return Some("Syncthing");
    }
    if bin == "dbus-daemon"
        || bin == "dbus-broker"
        || bin == "dbus-broker-launch"
        || bin == "xdg-dbus-proxy"
        || bin == "dbus-launch"
        || bin == "at-spi-bus-launcher"
        || bin == "at-spi2-registryd"
        || bin == "dbus-run-session"
        || bin == "dbus-monitor"
        || bin == "dbus-send"
        || bin == "gdbus"
        || (bin.starts_with("dbus-") && !bin.contains("dbus-python") && !bin.contains("dbus-test"))
    {
        return Some("Dbus");
    }
    if bin == "wl-paste"
        || bin == "wl-copy"
        || bin == "wl-clip-persist"
        || bin == "wl-clipboard"
        || bin == "cliphist"
        || bin == "copyq"
        || bin == "clipman"
        || bin == "clipse"
        || bin == "xclip"
        || bin == "xsel"
        || bin == "xclipboard"
        || bin == "gpaste-daemon"
        || bin == "gpaste"
        || bin == "greenclip"
        || bin == "clipcatd"
        || bin == "clipcatctl"
        || bin == "parcellite"
        || bin == "clipit"
        || bin == "diodon"
        || bin == "klipper"
        || bin == "autocutsel"
        || bin == "clipboard-sync"
    {
        return Some("Clipboard");
    }
    if bin == "pipewire"
        || bin == "pipewire-pulse"
        || bin == "wireplumber"
        || bin == "pipewire-media-session"
        || bin == "pulseaudio"
        || bin == "jackd"
        || bin == "jackdbus"
        || bin == "sndiod"
        || bin == "alsactl"
        || (bin.starts_with("jackd") && !bin.contains("jackdock"))
    {
        return Some("Audio Server");
    }
    None
}

/// Resolves a process's application group name by checking its own command line
/// and recursively traversing its parent process ancestry (`PPID`).
fn resolve_process_group(
    pid: u32,
    proc_map: &HashMap<u32, &ProcessInfo>,
    group_map: &mut HashMap<u32, &'static str>,
    visited: &mut HashSet<u32>,
) -> Option<&'static str> {
    if let Some(&g) = group_map.get(&pid) {
        return Some(g);
    }
    if !visited.insert(pid) {
        return None;
    }
    if let Some(p) = proc_map.get(&pid) {
        if let Some(g) = identify_process_group(&p.name) {
            group_map.insert(pid, g);
            return Some(g);
        }
        if p.ppid != 0
            && p.ppid != pid
            && let Some(parent_group) = resolve_process_group(p.ppid, proc_map, group_map, visited)
        {
            // Only propagate group to child if the parent group is an application suite that owns worker/tab sub-processes.
            // Infrastructure, launchers (like D-Bus), terminal emulators, audio servers, and clipboard managers must never absorb launched children.
            let propagates = matches!(
                parent_group,
                "LibreWolf"
                    | "Firefox"
                    | "Chrome"
                    | "Brave"
                    | "Edge"
                    | "Vesktop"
                    | "Discord"
                    | "Spotify"
                    | "Slack"
                    | "Obsidian"
                    | "Steam"
                    | "VS Code"
            );

            if propagates {
                group_map.insert(pid, parent_group);
                return Some(parent_group);
            }
        }
    }
    None
}

/// Aggregates related multi-process suites into single composite rows for the simple view,
/// sorting top-level items and placing expanded children directly beneath their parent header.
///
/// # Arguments
/// * `procs` - Slice of raw process entries.
/// * `expanded_groups` - Set of group names currently expanded into tree view.
/// * `sort_col` - Active column to sort top-level and child processes by.
/// * `sort_ascending` - Direction of sorting order.
///
/// # Returns
/// Vector of grouped, sorted, and cleanly nested `ProcessInfo` structs.
/// Strips tree branch markers (`▼ `, `▶ `, `├─ `, `└─ `) from a display name,
/// yielding the plain process or group label underneath.
pub fn strip_tree_markers(name: &str) -> &str {
    name.trim_start_matches("▼ ")
        .trim_start_matches("▶ ")
        .trim_start_matches("├─ ")
        .trim_start_matches("└─ ")
        .trim()
}

/// Computes a match relevance score for a process given a search query.
/// Higher score indicates higher match quality (exact/prefix matches on name/PID rank highest).
pub fn process_search_score(p: &ProcessInfo, query: &str) -> u32 {
    if query.is_empty() {
        return 1;
    }
    let q_lower = query.to_lowercase();
    let comm_lower = p.comm.to_lowercase();
    let name_lower = p.name.to_lowercase();
    let pid_str = p.pid.to_string();
    let user_lower = p.user.to_lowercase();

    // 1. PID exact/prefix match
    if pid_str == query {
        return 1000;
    }
    if pid_str.starts_with(query) {
        return 700;
    }

    // 2. Exact match on comm or name
    if comm_lower == q_lower || name_lower == q_lower {
        return 1000;
    }

    // Cleaned display name (strip tree branch markers)
    let clean_name = strip_tree_markers(&name_lower);

    if clean_name == q_lower {
        return 1000;
    }

    // 3. Prefix match on comm or clean_name
    if comm_lower.starts_with(&q_lower) || clean_name.starts_with(&q_lower) {
        return 800;
    }

    // 4. Substring match on comm or clean_name
    if comm_lower.contains(&q_lower) || clean_name.contains(&q_lower) {
        return 600;
    }

    // 5. Exact/prefix match on username
    if user_lower == q_lower {
        return 650;
    }
    if user_lower.starts_with(&q_lower) {
        return 550;
    }

    // 6. Substring match in full command line (e.g. arguments)
    if name_lower.contains(&q_lower) {
        return 400;
    }

    // 7. Fuzzy match on comm or clean_name
    if fuzzy_match(&q_lower, &comm_lower) || fuzzy_match(&q_lower, clean_name) {
        return 300;
    }

    // 8. Fuzzy match on user
    if fuzzy_match(&q_lower, &user_lower) {
        return 200;
    }

    // 9. Fuzzy match on full command line
    if fuzzy_match(&q_lower, &name_lower) {
        return 100;
    }

    0
}

/// Returns `true` if this process directly matches the search query (without checking group children).
pub fn directly_matches_search(p: &ProcessInfo, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    process_search_score(p, query) > 0
}

/// Evaluates whether a process (or any child in its group if it is a group header) matches the active search query.
pub fn matches_process_search(
    p: &ProcessInfo,
    query: &str,
    proc_map: Option<&HashMap<u32, &ProcessInfo>>,
) -> bool {
    if directly_matches_search(p, query) {
        return true;
    }
    if p.is_group_header
        && let Some(map) = proc_map
    {
        return p.grouped_pids.iter().any(|&pid| {
            if let Some(target) = map.get(&pid) {
                directly_matches_search(target, query)
            } else {
                fuzzy_match(query, &pid.to_string())
            }
        });
    }
    false
}

/// Groups process threads and child trees into clean top-level categories (e.g. LibreWolf tabs, Docker containers).
///
/// # Arguments
/// * `procs` - Slice of raw process entries.
/// * `expanded_groups` - Set of group names currently expanded into tree view.
/// * `sort_col` - Active column to sort top-level and child processes by.
/// * `sort_ascending` - Direction of sorting order.
/// * `search_query` - Current search/filter string, auto-expanding groups with matching children.
///
/// # Returns
/// Vector of grouped, sorted, and cleanly nested `ProcessInfo` structs.
pub fn group_processes_for_simple_view(
    procs: &[ProcessInfo],
    expanded_groups: &HashSet<String>,
    sort_col: ProcessSortColumn,
    sort_ascending: bool,
    search_query: &str,
) -> Vec<ProcessInfo> {
    let proc_map: HashMap<u32, &ProcessInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut group_map: HashMap<u32, &'static str> = HashMap::new();

    let mut groups: HashMap<&'static str, Vec<&ProcessInfo>> = HashMap::new();
    let mut ungrouped: Vec<&ProcessInfo> = Vec::new();

    for p in procs {
        let mut visited = HashSet::new();
        if let Some(group_name) =
            resolve_process_group(p.pid, &proc_map, &mut group_map, &mut visited)
        {
            groups.entry(group_name).or_default().push(p);
        } else {
            ungrouped.push(p);
        }
    }

    let mut top_level: Vec<ProcessInfo> = Vec::with_capacity(groups.len() + ungrouped.len());

    // Build aggregated group entries
    for (group_name, list) in &groups {
        if list.is_empty() {
            continue;
        }
        let root = list.iter().min_by_key(|p| p.pid).copied().unwrap();
        // The table reports RSS for every row, so a group's value is the sum of
        // its members' RSS values as well. Shared pages can therefore be counted
        // more than once, as RSS semantics require.
        let total_rss_kb: u64 = list.iter().map(|p| p.rss_kb).sum();
        let total_pss_kb = sum_optional_u64(list.iter().map(|p| p.pss_kb));
        let total_cpu_pct: f64 = list.iter().map(|p| p.cpu_percent).sum();
        let mut group_engine_percent: HashMap<String, f64> = HashMap::new();
        for p in list {
            if let Some(sample) = &p.gpu_sample {
                for (engine, percent) in &sample.engine_percent {
                    *group_engine_percent.entry(engine.clone()).or_default() += percent;
                }
            }
        }
        let total_gpu_pct = group_engine_percent
            .values()
            .copied()
            .reduce(f64::max)
            .map(|value| value.clamp(0.0, 100.0));
        let total_gpu_mem_kb = sum_optional_u64(list.iter().map(|p| p.gpu_mem_kb));
        let total_read_speed: f64 = list.iter().map(|p| p.read_speed).sum();
        let total_write_speed: f64 = list.iter().map(|p| p.write_speed).sum();
        let total_net_rx_speed = list
            .iter()
            .filter_map(|p| p.net_rx_speed)
            .reduce(|a, b| a + b);
        let total_net_tx_speed = list
            .iter()
            .filter_map(|p| p.net_tx_speed)
            .reduce(|a, b| a + b);
        let net_partial = list
            .iter()
            .any(|p| p.net_rx_speed.is_none() || p.net_tx_speed.is_none() || p.net_partial);
        let total_threads: u32 = list.iter().map(|p| p.threads).sum();
        let all_pids: Vec<u32> = list.iter().map(|p| p.pid).collect();

        if list.len() == 1 {
            let mut clean_p = root.clone();
            clean_p.name = clean_process_command(&root.name).into();
            clean_p.grouped_pids = vec![root.pid];
            clean_p.is_group_header = false;
            clean_p.is_group_child = false;
            clean_p.group_name = None;
            top_level.push(clean_p);
            continue;
        }

        let has_matching_child = !search_query.is_empty()
            && list.iter().any(|p| {
                fuzzy_match(search_query, &p.name)
                    || fuzzy_match(search_query, &p.comm)
                    || fuzzy_match(search_query, &p.pid.to_string())
                    || fuzzy_match(search_query, &p.user)
            });
        let is_expanded = expanded_groups.contains(*group_name) || has_matching_child;
        let arrow = if is_expanded { "▼ " } else { "▶ " };

        let base_title = match *group_name {
            "LibreWolf" => {
                let tab_count = list
                    .iter()
                    .filter(|p| {
                        p.comm.as_ref() == "Isolated Web Co"
                            || p.name.starts_with("Isolated Web Co")
                    })
                    .count();
                if tab_count > 1 {
                    format!("LibreWolf [{} Tabs]", tab_count)
                } else if tab_count == 1 {
                    "LibreWolf [1 Tab]".to_string()
                } else {
                    "LibreWolf".to_string()
                }
            }
            "Firefox" => {
                let tab_count = list
                    .iter()
                    .filter(|p| {
                        p.comm.as_ref() == "Isolated Web Co"
                            || p.name.starts_with("Isolated Web Co")
                    })
                    .count();
                if tab_count > 1 {
                    format!("Firefox [{} Tabs]", tab_count)
                } else if tab_count == 1 {
                    "Firefox [1 Tab]".to_string()
                } else {
                    "Firefox".to_string()
                }
            }
            "Docker" => {
                let container_count = list
                    .iter()
                    .filter(|p| {
                        p.comm.contains("containerd-shim") || p.name.contains("containerd-shim")
                    })
                    .count();
                let count = if container_count > 0 {
                    container_count
                } else {
                    list.len()
                };
                if count > 1 {
                    format!("Docker [{} Containers]", count)
                } else if count == 1 {
                    "Docker [1 Container]".to_string()
                } else {
                    "Docker".to_string()
                }
            }
            other => other.to_string(),
        };

        let header_name = format!("{arrow}{base_title}");

        top_level.push(ProcessInfo {
            pid: root.pid,
            ppid: root.ppid,
            comm: root.comm.clone(),
            name: header_name.into(),
            state: if list.iter().any(|p| p.state == "R") {
                "R".to_string()
            } else {
                root.state.clone()
            },
            rss_kb: total_rss_kb,
            starttime_ticks: root.starttime_ticks,
            pss_kb: total_pss_kb,
            utime: root.utime,
            stime: root.stime,
            read_bytes: root.read_bytes,
            write_bytes: root.write_bytes,
            threads: total_threads,
            uid: root.uid,
            user: root.user.clone(),
            cpu_percent: total_cpu_pct,
            read_speed: total_read_speed,
            write_speed: total_write_speed,
            net_rx_speed: total_net_rx_speed,
            net_tx_speed: total_net_tx_speed,
            net_partial,
            gpu_percent: total_gpu_pct,
            gpu_sample: (!group_engine_percent.is_empty()).then(|| {
                Box::new(GpuProcessSample {
                    engine_percent: group_engine_percent,
                    ..Default::default()
                })
            }),
            gpu_mem_kb: total_gpu_mem_kb,
            grouped_pids: all_pids,
            is_group_header: true,
            is_group_child: false,
            group_name: Some(group_name.to_string()),
        });
    }

    // Build cleaned ungrouped entries
    for p in ungrouped {
        let mut clean_p = p.clone();
        clean_p.name = clean_process_command(&p.name).into();
        clean_p.grouped_pids = vec![p.pid];
        clean_p.is_group_header = false;
        clean_p.is_group_child = false;
        clean_p.group_name = None;
        top_level.push(clean_p);
    }

    // Sort top-level entries first
    sort_processes(&mut top_level, sort_col, sort_ascending);

    // Build final nested tree structure
    let mut result = Vec::with_capacity(procs.len());
    for item in top_level {
        let grp_opt = item.group_name.clone();
        let is_hdr = item.is_group_header;
        result.push(item);

        let has_matching_child = !search_query.is_empty()
            && grp_opt
                .as_ref()
                .and_then(|g| groups.get(g.as_str()))
                .map(|child_list| {
                    child_list.iter().any(|p| {
                        fuzzy_match(search_query, &p.name)
                            || fuzzy_match(search_query, &p.comm)
                            || fuzzy_match(search_query, &p.pid.to_string())
                            || fuzzy_match(search_query, &p.user)
                    })
                })
                .unwrap_or(false);
        let is_expanded = grp_opt
            .as_ref()
            .map(|g| expanded_groups.contains(g))
            .unwrap_or(false)
            || has_matching_child;

        if is_hdr
            && let Some(ref grp_str) = grp_opt
            && is_expanded
            && let Some(child_list) = groups.get(grp_str.as_str())
        {
            let mut children: Vec<ProcessInfo> = child_list.iter().map(|&p| p.clone()).collect();
            sort_processes(&mut children, sort_col, sort_ascending);
            let num_children = children.len();
            for (c_idx, child) in children.into_iter().enumerate() {
                let branch = if c_idx + 1 == num_children {
                    "  └─ "
                } else {
                    "  ├─ "
                };
                let raw_clean = clean_process_command(&child.name);
                let child_clean = if raw_clean.is_empty() {
                    child.comm.clone()
                } else {
                    raw_clean.into()
                };
                let child_display = format!("{branch}{child_clean}");

                let mut child_info = child;
                child_info.name = child_display.into();
                child_info.grouped_pids = vec![child_info.pid];
                child_info.is_group_header = false;
                child_info.is_group_child = true;
                child_info.group_name = Some(grp_str.clone());
                result.push(child_info);
            }
        }
    }

    result
}

/// Validates if a target process still exists at `target.pid` and has not been replaced by an unrelated process.
///
/// # Arguments
/// * `target` - Target process details including expected PID, comm, and name.
///
/// # Returns
/// `Ok(())` if the process exists and matches the expected executable, or an `Err(String)` describing the mismatch.
pub fn validate_process_target(target: &ProcessTarget) -> Result<(), String> {
    let comm_path = format!("/proc/{}/comm", target.pid);
    let Ok(current_comm_raw) = fs::read_to_string(&comm_path) else {
        let display_name = if !target.comm.is_empty() {
            &target.comm
        } else {
            &target.name
        };
        return Err(format!(
            "Process '{}' (PID: {}) no longer exists.",
            display_name, target.pid
        ));
    };

    let current_comm = current_comm_raw.trim();
    if current_comm.is_empty() {
        let display_name = if !target.comm.is_empty() {
            &target.comm
        } else {
            &target.name
        };
        return Err(format!(
            "Process '{}' (PID: {}) no longer exists.",
            display_name, target.pid
        ));
    }

    let clean_exp_comm = strip_tree_markers(&target.comm);
    let clean_exp_name = strip_tree_markers(&target.name);

    let exp_comm_short: String = clean_exp_comm.chars().take(15).collect();
    let cur_comm_short: String = current_comm.chars().take(15).collect();

    if current_comm == clean_exp_comm
        || cur_comm_short == exp_comm_short
        || clean_exp_name.starts_with(current_comm)
        || clean_exp_comm.starts_with(current_comm)
        || current_comm.starts_with(clean_exp_comm)
    {
        return Ok(());
    }

    if let Ok(cmdline_bytes) = fs::read(format!("/proc/{}/cmdline", target.pid))
        && !cmdline_bytes.is_empty()
    {
        let cmd = String::from_utf8_lossy(&cmdline_bytes)
            .replace('\0', " ")
            .trim()
            .to_string();
        if cmd == clean_exp_name
            || cmd.starts_with(clean_exp_comm)
            || clean_exp_name.starts_with(&cmd)
            || cmd.contains(clean_exp_comm)
        {
            return Ok(());
        }
    }

    Err(format!(
        "Process at PID {} has changed! Expected '{}', but found '{}'.",
        target.pid, clean_exp_comm, current_comm
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_process_target() {
        // Validation on self PID should succeed
        let my_pid = std::process::id();
        let target = ProcessTarget {
            pid: my_pid,
            comm: "toptop".to_string(),
            name: "toptop".to_string(),
        };
        // Even if cargo test runner has a slightly different binary name (e.g. toptop-xxx),
        // it starts with toptop
        assert!(validate_process_target(&target).is_ok());

        // Validation on non-existent PID should fail with NotFound error
        let fake_target = ProcessTarget {
            pid: 4_194_300,
            comm: "nonexistent_proc".to_string(),
            name: "nonexistent_proc".to_string(),
        };
        let res = validate_process_target(&fake_target);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("no longer exists"));
    }

    #[test]
    fn test_strip_binary_path() {
        assert_eq!(strip_binary_path("/usr/bin/git"), "git");
        assert_eq!(
            strip_binary_path("/nix/store/123-pipewire-1.6.8/bin/pipewire"),
            "pipewire"
        );
        assert_eq!(strip_binary_path(".pavucontrol-wrapped"), "pavucontrol");
        assert_eq!(strip_binary_path("obsidian.wrapped"), "obsidian");
    }

    #[test]
    fn test_clean_process_command() {
        assert_eq!(
            clean_process_command("/usr/bin/python3 /home/user/script.py --arg1"),
            "script.py (python3) --arg1"
        );
        assert_eq!(
            clean_process_command("/usr/bin/node /app/server.js -p 8080"),
            "server.js (node) -p 8080"
        );
        assert_eq!(
            clean_process_command(
                "/nix/store/123-electron/electron /nix/store/456-obsidian/share/obsidian/app.asar"
            ),
            "obsidian (electron)"
        );
        assert_eq!(
            clean_process_command("java -jar /opt/minecraft/server.jar nogui"),
            "server.jar (java) nogui"
        );
        assert_eq!(
            clean_process_command("/nix/store/789-git/bin/git status"),
            "git status"
        );
        assert_eq!(
            clean_process_command("[kworker/u48:2-sdma1]"),
            "[kworker/u48:2-sdma1]"
        );
    }

    #[test]
    fn test_group_processes_for_simple_view() {
        let procs = vec![
            ProcessInfo {
                pid: 100,
                comm: "Isolated Web Co".into(),
                name: "/nix/store/123-librewolf/lib/librewolf/librewolf".into(),
                cpu_percent: 5.0,
                rss_kb: 500_000,
                pss_kb: Some(300_000),
                threads: 30,
                ..Default::default()
            },
            ProcessInfo {
                pid: 105,
                comm: "Isolated Web Co".into(),
                name: "/nix/store/123-librewolf/lib/librewolf/librewolf -contentproc 1 tab".into(),
                cpu_percent: 10.0,
                rss_kb: 300_000,
                pss_kb: Some(150_000),
                threads: 15,
                ..Default::default()
            },
            ProcessInfo {
                pid: 200,
                name: "/usr/bin/git status".into(),
                cpu_percent: 0.1,
                rss_kb: 10_000,
                pss_kb: Some(10_000),
                threads: 1,
                ..Default::default()
            },
            ProcessInfo {
                pid: 300,
                name: "/nix/store/123-electron/electron /nix/store/456-vesktop/resources/app.asar"
                    .into(),
                cpu_percent: 2.0,
                rss_kb: 150_000,
                pss_kb: Some(100_000),
                threads: 10,
                ..Default::default()
            },
            ProcessInfo {
                pid: 301,
                ppid: 300,
                name: "/nix/store/123-electron/electron --type=zygote".into(),
                cpu_percent: 1.0,
                rss_kb: 80_000,
                pss_kb: Some(40_000),
                threads: 4,
                ..Default::default()
            },
        ];

        let collapsed_set = HashSet::new();
        let grouped = group_processes_for_simple_view(
            &procs,
            &collapsed_set,
            ProcessSortColumn::Cpu,
            false,
            "",
        );
        assert_eq!(grouped.len(), 3);

        let librewolf = grouped
            .iter()
            .find(|p| p.name.contains("LibreWolf"))
            .unwrap();
        assert_eq!(librewolf.pid, 100);
        assert_eq!(librewolf.name.as_ref(), "▶ LibreWolf [2 Tabs]");
        assert_eq!(librewolf.cpu_percent, 15.0);
        assert_eq!(librewolf.rss_kb, 800_000);
        assert_eq!(librewolf.pss_kb, Some(450_000));
        assert_eq!(librewolf.threads, 45);
        assert_eq!(librewolf.grouped_pids, vec![100, 105]);

        let vesktop = grouped.iter().find(|p| p.name.contains("Vesktop")).unwrap();
        assert_eq!(vesktop.pid, 300);
        assert_eq!(vesktop.name.as_ref(), "▶ Vesktop");
        assert_eq!(vesktop.cpu_percent, 3.0);
        assert_eq!(vesktop.rss_kb, 230_000);
        assert_eq!(vesktop.pss_kb, Some(140_000));
        assert_eq!(vesktop.threads, 14);
        assert_eq!(vesktop.grouped_pids, vec![300, 301]);

        let git = grouped.iter().find(|p| p.name.contains("git")).unwrap();
        assert_eq!(git.name.as_ref(), "git status");
        assert_eq!(git.grouped_pids, vec![200]);

        // Test expanded state
        let mut expanded_set = HashSet::new();
        expanded_set.insert("LibreWolf".to_string());
        let expanded = group_processes_for_simple_view(
            &procs,
            &expanded_set,
            ProcessSortColumn::Cpu,
            false,
            "",
        );
        assert_eq!(expanded.len(), 5); // 1 header + 2 children for LibreWolf + 1 Vesktop + 1 git
        let lw_idx = expanded
            .iter()
            .position(|p| p.name.starts_with("▼ LibreWolf"))
            .unwrap();
        assert!(expanded[lw_idx + 1].name.starts_with("  ├─"));
        assert!(expanded[lw_idx + 2].name.starts_with("  └─"));

        // Test search auto-expansion on child match
        let searched = group_processes_for_simple_view(
            &procs,
            &collapsed_set,
            ProcessSortColumn::Cpu,
            false,
            "Isolated",
        );
        assert_eq!(searched.len(), 5); // LibreWolf auto-expands because child matched "Isolated"
        let searched_lw = searched
            .iter()
            .find(|p| p.name.starts_with("▼ LibreWolf"))
            .unwrap();
        assert_eq!(searched_lw.pid, 100);
    }

    #[test]
    fn test_identify_process_group() {
        assert_eq!(identify_process_group("syncthing"), Some("Syncthing"));
        assert_eq!(
            identify_process_group("/usr/bin/syncthing --no-browser"),
            Some("Syncthing")
        );
        assert_eq!(
            identify_process_group("/nix/store/abc-syncthing-1.27.0/bin/syncthing"),
            Some("Syncthing")
        );
        assert_eq!(
            identify_process_group("syncthing-inotify"),
            Some("Syncthing")
        );
        assert_eq!(identify_process_group("dbus-daemon --system"), Some("Dbus"));
        assert_eq!(
            identify_process_group("dbus-broker-launch --scope user"),
            Some("Dbus")
        );
        assert_eq!(
            identify_process_group("/nix/store/123-dbus-broker/bin/dbus-broker"),
            Some("Dbus")
        );
        assert_eq!(
            identify_process_group("xdg-dbus-proxy --args"),
            Some("Dbus")
        );
        assert_eq!(
            identify_process_group("wl-paste --type text --watch cliphist store"),
            Some("Clipboard")
        );
        assert_eq!(identify_process_group("wl-copy"), Some("Clipboard"));
        assert_eq!(
            identify_process_group("/usr/bin/copyq --start-server"),
            Some("Clipboard")
        );
        assert_eq!(
            identify_process_group("xclip -selection clipboard"),
            Some("Clipboard")
        );
        assert_eq!(identify_process_group("gpaste-daemon"), Some("Clipboard"));
        assert_eq!(identify_process_group("clipman"), Some("Clipboard"));
        assert_eq!(
            identify_process_group("greenclip daemon"),
            Some("Clipboard")
        );
        assert_eq!(
            identify_process_group("/nix/store/123-pipewire/bin/pipewire"),
            Some("Audio Server")
        );
        assert_eq!(
            identify_process_group("pipewire-pulse"),
            Some("Audio Server")
        );
        assert_eq!(identify_process_group("wireplumber"), Some("Audio Server"));
        assert_eq!(
            identify_process_group("/usr/bin/pulseaudio --daemonize=no"),
            Some("Audio Server")
        );
        assert_eq!(
            identify_process_group("jackd -d alsa"),
            Some("Audio Server")
        );
        assert_eq!(identify_process_group("sndiod"), Some("Audio Server"));

        // Processes that must NEVER be grouped under Dbus
        assert_eq!(
            identify_process_group("llama-server -m /models/llama-3.2-3b.gguf --port 8080"),
            None
        );
        assert_eq!(identify_process_group("llama-server --dbus-service"), None);
        assert_eq!(
            identify_process_group("lm-studio --enable-features=UseDBus"),
            None
        );
        assert_eq!(identify_process_group("lmstudio"), None);
        assert_eq!(
            identify_process_group("node /home/user/app/dbus/server.js"),
            None
        );
        assert_eq!(identify_process_group("python3 -m dbus_next.service"), None);
    }

    #[test]
    fn test_dbus_does_not_absorb_unrelated_processes() {
        let dbus_proc = ProcessInfo {
            pid: 500,
            ppid: 1,
            comm: "dbus-daemon".into(),
            name: "dbus-daemon --system".into(),
            state: "S".to_string(),
            rss_kb: 4096,
            cpu_percent: 0.1,
            ..Default::default()
        };
        let llama_proc = ProcessInfo {
            pid: 1200,
            ppid: 500, // Spawned via D-Bus activation or child of dbus-daemon
            comm: "llama-server".into(),
            name: "llama-server -m /models/test.gguf".into(),
            state: "R".to_string(),
            rss_kb: 8192000,
            cpu_percent: 25.0,
            ..Default::default()
        };
        let node_proc = ProcessInfo {
            pid: 1201,
            ppid: 500,
            comm: "node".into(),
            name: "node /app/server.js".into(),
            state: "S".to_string(),
            rss_kb: 65536,
            cpu_percent: 1.0,
            ..Default::default()
        };

        let procs = vec![dbus_proc, llama_proc, node_proc];
        let expanded = HashSet::new();
        let grouped =
            group_processes_for_simple_view(&procs, &expanded, ProcessSortColumn::Cpu, false, "");

        // llama-server and node must be top-level standalone entries, NOT grouped inside Dbus
        let llama_entry = grouped
            .iter()
            .find(|p| p.pid == 1200)
            .expect("llama-server should exist");
        assert!(!llama_entry.is_group_child);
        assert_eq!(llama_entry.group_name, None);

        let node_entry = grouped
            .iter()
            .find(|p| p.pid == 1201)
            .expect("node should exist");
        assert!(!node_entry.is_group_child);
        assert_eq!(node_entry.group_name, None);
    }

    #[test]
    fn test_read_process_detail() {
        let current_pid = std::process::id();
        let proc_info = ProcessInfo {
            pid: current_pid,
            ppid: 1,
            comm: "toptop_test".into(),
            name: "toptop_test".into(),
            state: "R".to_string(),
            rss_kb: 1024,
            cpu_percent: 5.0,
            ..Default::default()
        };
        let mut proc_map = HashMap::new();
        proc_map.insert(current_pid, &proc_info);

        let detail = read_process_detail(&proc_info, &proc_map, false);
        assert_eq!(detail.info.pid, current_pid);
        assert_eq!(detail.info.comm.as_ref(), "toptop_test");
        assert!(!detail.cmdline.is_empty());
        assert!(!detail.cwd_path.is_empty());
        assert!(detail.open_fds > 0);
    }

    #[test]
    fn test_parse_drm_memory_kb() {
        assert_eq!(
            parse_drm_memory_kb("drm-resident-vram:\t6279292 KiB"),
            6279292
        );
        assert_eq!(parse_drm_memory_kb("drm-resident-gtt:\t2 MiB"), 2048);
        assert_eq!(parse_drm_memory_kb("drm-resident-vram:\t6.8 GiB"), 7130317);
        assert_eq!(parse_drm_memory_kb("drm-total-cpu:\t0"), 0);
        assert_eq!(parse_drm_memory_kb("drm-resident-vram: 12 KiB"), 12);
        assert_eq!(parse_drm_memory_kb("drm-memory-local:\t512 MiB"), 524288);
        assert_eq!(parse_drm_memory_kb("drm-total-vram:\t1024 B"), 1);
    }
}
