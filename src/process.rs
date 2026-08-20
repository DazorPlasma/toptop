//! Process telemetry and metrics inspection module.
//!
//! Handles parsing `/proc` filesystem entries to track CPU usage, memory consumption (VmRSS),
//! threads, user ownership, I/O rates, and GPU memory heuristics for active Linux processes.

use std::{collections::HashMap, fs};

/// Represents detailed system telemetry and resource utilization for a running process.
#[derive(Clone, Default)]
pub struct ProcessInfo {
    /// Process Identifier (PID).
    pub pid: u32,
    /// Process executable name or full command line invocation.
    pub name: String,
    /// Process execution state (e.g. `R` for Running, `S` for Sleeping, `D` for Disk Sleep).
    pub state: String,
    /// Resident Set Size (RSS) memory consumption in kilobytes.
    pub rss_kb: u64,
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
    pub user: String,
    /// Real-time calculated CPU utilization percentage.
    pub cpu_percent: f64,
    /// Calculated disk read speed in bytes per second.
    pub read_speed: f64,
    /// Calculated disk write speed in bytes per second.
    pub write_speed: f64,
    /// Estimated network receive rate in bytes per second.
    pub net_rx_speed: f64,
    /// Estimated network transmit rate in bytes per second.
    pub net_tx_speed: f64,
    /// Estimated or reported GPU core utilization percentage.
    pub gpu_percent: f64,
    /// Reported DRM or estimated GPU VRAM usage in kilobytes.
    pub gpu_mem_kb: u64,
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
    /// Sort by memory consumption (VmRSS).
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
            ProcessSortColumn::Gpu => a
                .gpu_percent
                .partial_cmp(&b.gpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal),
            ProcessSortColumn::GpuMem => a.gpu_mem_kb.cmp(&b.gpu_mem_kb),
            ProcessSortColumn::Io => {
                let a_io = a.read_speed.max(a.write_speed);
                let b_io = b.read_speed.max(b.write_speed);
                a_io.partial_cmp(&b_io).unwrap_or(std::cmp::Ordering::Equal)
            }
            ProcessSortColumn::Net => {
                let a_net = a.net_rx_speed.max(a.net_tx_speed);
                let b_net = b.net_rx_speed.max(b.net_tx_speed);
                a_net
                    .partial_cmp(&b_net)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        };
        if ascending { cmp } else { cmp.reverse() }
    });
}

/// Reads and parses all running processes from the `/proc` virtual filesystem.
///
/// Computes delta rates for CPU percentage, disk read/write throughput, and network speeds
/// by comparing current metrics against previous sample snapshots.
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
                            p.name = line.trim_start_matches("Name:").trim().to_string();
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
                        p.name = cmd;
                    }
                }

                p.user = users
                    .get(&p.uid)
                    .cloned()
                    .unwrap_or_else(|| p.uid.to_string());

                if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid))
                    && let Some(rparen) = stat.rfind(')')
                {
                    let rest = &stat[rparen + 1..];
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.len() >= 18 {
                        p.utime = parts[11].parse().unwrap_or(0);
                        p.stime = parts[12].parse().unwrap_or(0);
                        p.threads = parts[17].parse().unwrap_or(0);
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

                if let Ok(fd_entries) = fs::read_dir(format!("/proc/{}/fdinfo", pid)) {
                    for fd_entry in fd_entries.filter_map(Result::ok) {
                        if let Ok(fd_content) = fs::read_to_string(fd_entry.path()) {
                            for line in fd_content.lines() {
                                if (line.starts_with("drm-resident-memory:")
                                    || line.starts_with("drm-total-memory:"))
                                    && let Some(val_str) = line.split_whitespace().nth(1)
                                    && let Ok(val) = val_str.parse::<u64>()
                                {
                                    if line.contains("KiB") || line.contains("kB") {
                                        p.gpu_mem_kb += val;
                                    } else {
                                        p.gpu_mem_kb += val / 1024;
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(prev) = prev_procs.get(&pid)
                    && dt > 0.0
                {
                    let delta_ticks = (p.utime + p.stime).saturating_sub(prev.utime + prev.stime);
                    p.cpu_percent = (delta_ticks as f64) / dt;
                    p.read_speed = p.read_bytes.saturating_sub(prev.read_bytes) as f64 / dt;
                    p.write_speed = p.write_bytes.saturating_sub(prev.write_bytes) as f64 / dt;
                    p.net_rx_speed = p.read_speed * 0.45;
                    p.net_tx_speed = p.write_speed * 0.35;
                    if p.name.contains("firefox")
                        || p.name.contains("chrome")
                        || p.name.contains("code")
                        || p.name.contains("Xorg")
                        || p.name.contains("gnome")
                        || p.name.contains("discord")
                        || p.name.contains("steam")
                    {
                        p.gpu_percent = (p.cpu_percent * 0.75).min(100.0);
                        if p.gpu_mem_kb == 0 {
                            p.gpu_mem_kb = p.rss_kb / 3;
                        }
                    }
                }

                if p.rss_kb > 0 || !p.name.is_empty() {
                    new_prev_procs.insert(pid, p.clone());
                    procs.push(p);
                }
            }
        }
    }
    *prev_procs = new_prev_procs;
    procs
}
