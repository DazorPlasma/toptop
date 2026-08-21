//! Process telemetry and metrics inspection module.
//!
//! Handles parsing `/proc` filesystem entries to track CPU usage, memory consumption (VmRSS),
//! threads, user ownership, I/O rates, and GPU memory heuristics for active Linux processes.

use std::{
    collections::{HashMap, HashSet},
    fs,
};

use crate::utils::fuzzy_match;

/// Represents detailed system telemetry and resource utilization for a running process.
#[derive(Clone, Default)]
pub struct ProcessInfo {
    /// Process Identifier (PID).
    pub pid: u32,
    /// Parent Process Identifier (PPID).
    pub ppid: u32,
    /// Short executable command name from `/proc/[pid]/comm` or `/proc/[pid]/status`.
    pub comm: String,
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
    /// Proportional Set Size (PSS) true physical memory share in kilobytes.
    pub pss_kb: u64,
    /// List of PIDs contained in this group if aggregated in simple view.
    pub grouped_pids: Vec<u32>,
    /// Indicates whether this row represents a collapsible group header in simple view.
    pub is_group_header: bool,
    /// Indicates whether this row is an expanded child subprocess in simple view.
    pub is_group_child: bool,
    /// Identifier of the group this row belongs to.
    pub group_name: Option<String>,
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
    /// Signal to dispatch (`Signal::Term` for SIGTERM, `Signal::Kill` for SIGKILL).
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
                            p.comm = line.trim_start_matches("Name:").trim().to_string();
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
                        p.ppid = parts[1].parse().unwrap_or(0);
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

                if let Ok(smaps) = fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)) {
                    for line in smaps.lines() {
                        if line.starts_with("Pss:") {
                            let mut parts = line.split_whitespace();
                            parts.next();
                            p.pss_kb = parts.next().unwrap_or("0").parse().unwrap_or(0);
                            break;
                        }
                    }
                }
                if p.pss_kb == 0 {
                    p.pss_kb = p.rss_kb;
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
                        || p.name.contains("vscode")
                        || p.name.contains("codium")
                        || p.name.contains("vesktop")
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

/// Reads deep diagnostic telemetry for a specific process from `/proc/[pid]`.
///
/// # Arguments
/// * `proc_info` - Baseline process info snapshot.
/// * `proc_map` - Map of currently known processes for parent lookups.
///
/// # Returns
/// A populated `ProcessDetailInfo` structure.
pub fn read_process_detail(
    proc_info: &ProcessInfo,
    proc_map: &HashMap<u32, &ProcessInfo>,
) -> ProcessDetailInfo {
    let pid = proc_info.pid;
    let mut detail = ProcessDetailInfo {
        info: proc_info.clone(),
        ..Default::default()
    };

    if let Some(parent) = proc_map.get(&proc_info.ppid) {
        detail.parent_comm = parent.comm.clone();
    } else if let Ok(comm) = fs::read_to_string(format!("/proc/{}/comm", proc_info.ppid)) {
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
        detail.cmdline = proc_info.name.clone();
    }

    if let Ok(entries) = fs::read_dir(format!("/proc/{}/fd", pid)) {
        detail.open_fds = entries.filter_map(Result::ok).count();
    }

    if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid))
        && let Some(rparen) = stat.rfind(')')
    {
        let rest = &stat[rparen + 1..];
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() >= 17 {
            detail.priority = parts[15].parse().unwrap_or(0);
            detail.nice = parts[16].parse().unwrap_or(0);
        }
    }

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
    if lower.contains("rust-analyzer") || lower.contains("rust_analyzer") {
        return Some("Rust Analyzer");
    }
    if lower.contains("gvfs") || bin.contains("gvfs") {
        return Some("Gnome Virtual FileSystem");
    }
    if lower.contains("dockerd")
        || lower.contains("containerd")
        || lower.contains("docker-proxy")
        || bin == "docker"
    {
        return Some("Docker");
    }
    if lower.contains("syncthing") || bin.contains("syncthing") {
        return Some("Syncthing");
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
            && let Some(g) = resolve_process_group(p.ppid, proc_map, group_map, visited)
        {
            group_map.insert(pid, g);
            return Some(g);
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
    let clean_name = name_lower
        .trim_start_matches("▼ ")
        .trim_start_matches("▶ ")
        .trim_start_matches("├─ ")
        .trim_start_matches("└─ ")
        .trim();

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
        if let Some(group_name) = resolve_process_group(p.pid, &proc_map, &mut group_map, &mut visited) {
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
        let total_rss_kb: u64 = list.iter().map(|p| p.rss_kb).sum();
        let total_pss_kb: u64 = list.iter().map(|p| p.pss_kb).sum();
        let effective_mem_kb = if total_pss_kb > 0 {
            total_pss_kb
        } else {
            total_rss_kb
        };
        let total_cpu_pct: f64 = list.iter().map(|p| p.cpu_percent).sum();
        let total_gpu_pct: f64 = list.iter().map(|p| p.gpu_percent).sum();
        let total_gpu_mem_kb: u64 = list.iter().map(|p| p.gpu_mem_kb).sum();
        let total_read_speed: f64 = list.iter().map(|p| p.read_speed).sum();
        let total_write_speed: f64 = list.iter().map(|p| p.write_speed).sum();
        let total_net_rx_speed: f64 = list.iter().map(|p| p.net_rx_speed).sum();
        let total_net_tx_speed: f64 = list.iter().map(|p| p.net_tx_speed).sum();
        let total_threads: u32 = list.iter().map(|p| p.threads).sum();
        let all_pids: Vec<u32> = list.iter().map(|p| p.pid).collect();

        if list.len() == 1 {
            let mut clean_p = root.clone();
            clean_p.name = clean_process_command(&root.name);
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
                    .filter(|p| p.comm == "Isolated Web Co" || p.name.starts_with("Isolated Web Co"))
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
                    .filter(|p| p.comm == "Isolated Web Co" || p.name.starts_with("Isolated Web Co"))
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
                        p.comm.contains("containerd-shim")
                            || p.name.contains("containerd-shim")
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
            name: header_name,
            state: if list.iter().any(|p| p.state == "R") {
                "R".to_string()
            } else {
                root.state.clone()
            },
            rss_kb: effective_mem_kb,
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
            gpu_percent: total_gpu_pct,
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
        clean_p.name = clean_process_command(&p.name);
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
                    raw_clean
                };
                let child_display = format!("{branch}{child_clean}");

                let mut child_info = child;
                child_info.name = child_display;
                child_info.rss_kb = if child_info.pss_kb > 0 { child_info.pss_kb } else { child_info.rss_kb };
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

    let clean_exp_comm = target
        .comm
        .trim_start_matches("▼ ")
        .trim_start_matches("▶ ")
        .trim_start_matches("├─ ")
        .trim_start_matches("└─ ")
        .trim();

    let clean_exp_name = target
        .name
        .trim_start_matches("▼ ")
        .trim_start_matches("▶ ")
        .trim_start_matches("├─ ")
        .trim_start_matches("└─ ")
        .trim();

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
                comm: "Isolated Web Co".to_string(),
                name: "/nix/store/123-librewolf/lib/librewolf/librewolf".to_string(),
                cpu_percent: 5.0,
                rss_kb: 500_000,
                pss_kb: 300_000,
                threads: 30,
                ..Default::default()
            },
            ProcessInfo {
                pid: 105,
                comm: "Isolated Web Co".to_string(),
                name: "/nix/store/123-librewolf/lib/librewolf/librewolf -contentproc 1 tab".to_string(),
                cpu_percent: 10.0,
                rss_kb: 300_000,
                pss_kb: 150_000,
                threads: 15,
                ..Default::default()
            },
            ProcessInfo {
                pid: 200,
                name: "/usr/bin/git status".to_string(),
                cpu_percent: 0.1,
                rss_kb: 10_000,
                pss_kb: 10_000,
                threads: 1,
                ..Default::default()
            },
            ProcessInfo {
                pid: 300,
                name: "/nix/store/123-electron/electron /nix/store/456-vesktop/resources/app.asar".to_string(),
                cpu_percent: 2.0,
                rss_kb: 150_000,
                pss_kb: 100_000,
                threads: 10,
                ..Default::default()
            },
            ProcessInfo {
                pid: 301,
                ppid: 300,
                name: "/nix/store/123-electron/electron --type=zygote".to_string(),
                cpu_percent: 1.0,
                rss_kb: 80_000,
                pss_kb: 40_000,
                threads: 4,
                ..Default::default()
            },
        ];

        let collapsed_set = HashSet::new();
        let grouped = group_processes_for_simple_view(&procs, &collapsed_set, ProcessSortColumn::Cpu, false, "");
        assert_eq!(grouped.len(), 3);

        let librewolf = grouped.iter().find(|p| p.name.contains("LibreWolf")).unwrap();
        assert_eq!(librewolf.pid, 100);
        assert_eq!(librewolf.name, "▶ LibreWolf [2 Tabs]");
        assert_eq!(librewolf.cpu_percent, 15.0);
        assert_eq!(librewolf.rss_kb, 450_000);
        assert_eq!(librewolf.pss_kb, 450_000);
        assert_eq!(librewolf.threads, 45);
        assert_eq!(librewolf.grouped_pids, vec![100, 105]);

        let vesktop = grouped.iter().find(|p| p.name.contains("Vesktop")).unwrap();
        assert_eq!(vesktop.pid, 300);
        assert_eq!(vesktop.name, "▶ Vesktop");
        assert_eq!(vesktop.cpu_percent, 3.0);
        assert_eq!(vesktop.rss_kb, 140_000);
        assert_eq!(vesktop.pss_kb, 140_000);
        assert_eq!(vesktop.threads, 14);
        assert_eq!(vesktop.grouped_pids, vec![300, 301]);

        let git = grouped.iter().find(|p| p.name.contains("git")).unwrap();
        assert_eq!(git.name, "git status");
        assert_eq!(git.grouped_pids, vec![200]);

        // Test expanded state
        let mut expanded_set = HashSet::new();
        expanded_set.insert("LibreWolf".to_string());
        let expanded = group_processes_for_simple_view(&procs, &expanded_set, ProcessSortColumn::Cpu, false, "");
        assert_eq!(expanded.len(), 5); // 1 header + 2 children for LibreWolf + 1 Vesktop + 1 git
        let lw_idx = expanded
            .iter()
            .position(|p| p.name.starts_with("▼ LibreWolf"))
            .unwrap();
        assert!(expanded[lw_idx + 1].name.starts_with("  ├─"));
        assert!(expanded[lw_idx + 2].name.starts_with("  └─"));

        // Test search auto-expansion on child match
        let searched = group_processes_for_simple_view(&procs, &collapsed_set, ProcessSortColumn::Cpu, false, "Isolated");
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
        assert_eq!(identify_process_group("/usr/bin/syncthing --no-browser"), Some("Syncthing"));
        assert_eq!(identify_process_group("/nix/store/abc-syncthing-1.27.0/bin/syncthing"), Some("Syncthing"));
        assert_eq!(identify_process_group("syncthing-inotify"), Some("Syncthing"));
    }

    #[test]
    fn test_read_process_detail() {
        let current_pid = std::process::id();
        let proc_info = ProcessInfo {
            pid: current_pid,
            ppid: 1,
            comm: "toptop_test".to_string(),
            name: "toptop_test".to_string(),
            state: "R".to_string(),
            rss_kb: 1024,
            cpu_percent: 5.0,
            ..Default::default()
        };
        let mut proc_map = HashMap::new();
        proc_map.insert(current_pid, &proc_info);

        let detail = read_process_detail(&proc_info, &proc_map);
        assert_eq!(detail.info.pid, current_pid);
        assert_eq!(detail.info.comm, "toptop_test");
        assert!(!detail.cmdline.is_empty());
        assert!(!detail.cwd_path.is_empty());
        assert!(detail.open_fds > 0);
    }
}

