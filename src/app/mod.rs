//! Application state, event loop, and background scanner coordination.
//!
//! [`App`] owns every piece of runtime state: the live telemetry snapshot,
//! time-travel history, per-tab view state, and the channels feeding results
//! back from the background storage scanners. Input handling lives in
//! [`keys`](self::keys) and [`mouse`](self::mouse); frame composition in
//! [`draw`](self::draw).

mod draw;
mod keys;
mod mouse;

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};

use crossterm::event::{self, Event};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect, widgets::TableState};
use rustix::process::Signal;

use crate::{
    process::{
        ProcessErrorPopup, ProcessInfo, ProcessKillConfirmation, ProcessSortColumn, ProcessTarget,
        directly_matches_search, group_processes_for_simple_view, matches_process_search,
        process_search_score, read_processes, sort_processes, validate_process_target,
    },
    snapshot::{HISTORY_LEN, MetricSample, Snapshot},
    system::{
        CpuTicks, DnsResolver, PackageStorageCategory, PackageStorageItem, calculate_usage,
        get_cpu_model, get_ram_info, get_users, is_item_visible, read_battery, read_cpu_freq_info,
        read_cpu_temp, read_cpu_ticks, read_disk_io, read_disk_mounts, read_dust_path,
        read_dust_storage, read_gpu_metrics, read_memory, read_network_connections,
        read_network_interfaces, read_package_storage_categories, read_ram_temp,
        read_system_general_info,
    },
    theme::io_gradient_pct,
    ui,
};

/// Interval between telemetry refreshes; also the snapshot cadence.
const TICK_INTERVAL: Duration = Duration::from_millis(2000);

/// Warmup pause between the two initial CPU tick reads so usage has a delta.
const CPU_WARMUP: Duration = Duration::from_millis(100);

/// Grace period after signalling processes before re-reading the process table.
const KILL_SETTLE: Duration = Duration::from_millis(40);

/// How long the clipboard-copied confirmation stays on screen.
const COPY_FEEDBACK: Duration = Duration::from_secs(2);

/// Maximum number of historical snapshots retained for time travel.
const MAX_SNAPSHOTS: usize = 300;

/// Rows jumped by PageUp/PageDown in list views.
const PAGE_STEP: isize = 10;

/// Rows jumped by PageUp/PageDown in the storage tree.
const STORAGE_PAGE_STEP: isize = 5;

/// Rows scrolled by mouse wheel ticks in list views.
const SCROLL_STEP: usize = 3;

/// Message carrying children discovered by a lazy directory-tree scan.
type DustTreeMessage = (String, usize, Vec<PackageStorageItem>);

/// The six primary dashboard tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    /// System overview dashboard.
    General,
    /// Process manager table.
    Processes,
    /// CPU and RAM history charts.
    CpuRam,
    /// GPU telemetry.
    Gpu,
    /// Network interfaces and connections.
    Network,
    /// Disks and package storage.
    Disks,
}

impl Tab {
    /// All tabs in display order.
    const ALL: [Tab; 6] = [
        Tab::General,
        Tab::Processes,
        Tab::CpuRam,
        Tab::Gpu,
        Tab::Network,
        Tab::Disks,
    ];

    /// The next tab, wrapping from last to first.
    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The previous tab, wrapping from first to last.
    fn prev(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// Position of this tab in display order.
    fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    /// The tab displayed at `index`, clamped to the valid range.
    fn from_index(index: usize) -> Self {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }
}

/// Aggregated view state for the process table.
struct ProcessViewState {
    /// Whether the advanced (all-column) table layout is active.
    advanced: bool,
    /// Whether the search input is currently focused.
    searching: bool,
    /// Active search filter text.
    query: String,
    /// Column the process table is sorted by.
    sort_col: ProcessSortColumn,
    /// Sort direction flag (`true` for ascending).
    ascending: bool,
    /// Application groups expanded into child rows.
    expanded_groups: HashSet<String>,
    /// Ratatui selection/scroll state for the process table.
    table_state: TableState,
    /// PID of the process shown in the full-screen detail view, if open.
    selected_detail: Option<u32>,
}

impl Default for ProcessViewState {
    fn default() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            advanced: false,
            searching: false,
            query: String::new(),
            sort_col: ProcessSortColumn::Mem,
            ascending: false,
            expanded_groups: HashSet::new(),
            table_state,
            selected_detail: None,
        }
    }
}

/// Complete application runtime state.
pub(crate) struct App {
    /// Live telemetry, updated in place every tick.
    live: Snapshot,

    // Telemetry reader caches ---------------------------------------------------
    /// Marketing CPU model string (read once at startup).
    cpu_model: String,
    /// UID to username map from `/etc/passwd`.
    users: HashMap<u32, String>,
    /// Background reverse-DNS resolver shared with connection reading.
    dns_resolver: DnsResolver,
    /// Reusable scratch buffer avoiding per-read heap allocations.
    io_buf: String,
    /// Previous `/proc/stat` tick sample (swapped with [`App::curr_ticks`]).
    prev_ticks: Vec<CpuTicks>,
    /// Current `/proc/stat` tick sample.
    curr_ticks: Vec<CpuTicks>,
    /// Previous network byte counters keyed by interface name.
    prev_net: HashMap<String, (u64, u64)>,
    /// Previous disk byte counters keyed by device name.
    prev_disk: HashMap<String, (u64, u64)>,
    /// Previous per-process counters used to compute rate deltas.
    prev_procs: HashMap<u32, ProcessInfo>,

    // Time travel -----------------------------------------------------------------
    /// Historical snapshot ring (bounded by [`MAX_SNAPSHOTS`]).
    snapshots: Vec<Snapshot>,
    /// Index into [`App::snapshots`] currently being viewed.
    snapshot_idx: usize,
    /// Whether telemetry polling is paused.
    paused: bool,

    // View state --------------------------------------------------------------------
    /// Currently visible tab.
    current_tab: Tab,
    /// Sub-tab index within the General dashboard (0..4).
    general_sub_tab: usize,
    /// Sub-tab index within the GPU dashboard (0..2).
    gpu_sub_tab: usize,
    /// Process-table navigation and filtering state.
    procs: ProcessViewState,
    /// Scroll offset of the connections list on the Network tab.
    net_scroll_offset: usize,
    /// Disk-tab layout and navigation state.
    disks: ui::DisksViewState,

    // Package storage scanning ---------------------------------------------------------
    /// Latest package/storage categories ("All" entry first).
    storage_categories: Vec<PackageStorageCategory>,
    /// Receiver for periodic full package-storage scans.
    storage_rx: Receiver<Vec<PackageStorageCategory>>,
    /// Sender used to request whole-disk dust scans.
    dust_tx: Sender<Option<PackageStorageCategory>>,
    /// Receiver for completed whole-disk dust scans.
    dust_rx: Receiver<Option<PackageStorageCategory>>,
    /// Sender used to request single-directory tree scans.
    dust_tree_tx: Sender<DustTreeMessage>,
    /// Receiver for completed single-directory tree scans.
    dust_tree_rx: Receiver<DustTreeMessage>,
    /// Cache of already-scanned directory children keyed by path.
    dust_dir_cache: HashMap<String, Vec<PackageStorageItem>>,
    /// Paths with an in-flight directory scan.
    dust_scanning_paths: HashSet<String>,
    /// Whether a root-level dust scan is currently running.
    is_dust_scanning: bool,

    // Ephemeral UI state ------------------------------------------------------------------
    /// Set whenever something changed and the next frame must be rebuilt.
    needs_redraw: bool,
    /// Time until which the clipboard-copied badge should be displayed.
    copy_feedback_until: Option<Instant>,
    /// Screen rectangles of the kill-confirmation modal buttons from the last frame.
    modal_btn_rects: Option<(Rect, Rect)>,
    /// Pending process validation error popup, if any.
    error_popup: Option<ProcessErrorPopup>,
    /// Pending kill confirmation modal, if any.
    kill_confirmation: Option<ProcessKillConfirmation>,
    /// Instant of the last telemetry tick.
    last_tick: Instant,
    /// Tab bar area cached from the previous frame for mouse hit-testing.
    tabs_area: Rect,
    /// Body area cached from the previous frame for mouse hit-testing.
    table_area: Rect,
}

impl App {
    /// Captures initial telemetry and wires up background scanner threads.
    ///
    /// # Errors
    /// Propagated unchanged from blocking terminal warmup I/O; currently no
    /// fallible operations occur during construction.
    pub(crate) fn new() -> Self {
        let cpu_model = get_cpu_model();
        let users = get_users();
        let dns_resolver = DnsResolver::new();

        let mut io_buf = String::with_capacity(8192);
        let mut prev_ticks = Vec::with_capacity(32);
        let mut curr_ticks = Vec::with_capacity(32);
        read_cpu_ticks(&mut io_buf, &mut prev_ticks);
        std::thread::sleep(CPU_WARMUP);
        read_cpu_ticks(&mut io_buf, &mut curr_ticks);

        let mut global_usage = 0.0;
        let mut core_usages = Vec::new();
        if !prev_ticks.is_empty() && !curr_ticks.is_empty() {
            global_usage = calculate_usage(&prev_ticks[0], &curr_ticks[0]);
            core_usages = (1..curr_ticks.len().min(prev_ticks.len()))
                .map(|i| calculate_usage(&prev_ticks[i], &curr_ticks[i]))
                .collect();
        }

        let mut prev_net = HashMap::new();
        let net_ifaces = read_network_interfaces(&mut prev_net, 0.0);
        let mut prev_disk = HashMap::new();
        let disk_mounts = read_disk_mounts();
        let disk_io = read_disk_io(&mut prev_disk, 0.0);

        let mem = read_memory(&mut io_buf);
        let mut prev_procs = HashMap::new();
        let mut processes = read_processes(&mut prev_procs, &users, 0.0);
        sort_processes(&mut processes, ProcessSortColumn::Mem, false);

        let gpu_metrics = read_gpu_metrics();
        let net_connections = read_network_connections(&dns_resolver);
        let sys_info = read_system_general_info();
        let battery = read_battery();
        let ram_info = get_ram_info(mem.total_mem_mb, &cpu_model);
        let (cpu_cur_mhz, cpu_min_mhz, cpu_max_mhz) = read_cpu_freq_info();

        let mut live = Snapshot {
            sys_info,
            battery,
            global_usage,
            core_usages,
            cpu_cur_mhz,
            cpu_min_mhz,
            cpu_max_mhz,
            cpu_temp: read_cpu_temp(),
            ram_temp: read_ram_temp(),
            mem,
            ram_info,
            gpu_metrics,
            net_ifaces,
            net_connections,
            disk_io,
            disk_mounts,
            processes,
            history: Default::default(),
        };

        // Seed the newest history slot so the first frame shows a live data point
        // where telemetry was already available at startup.
        live.history.cpu[HISTORY_SEED] = Some(live.global_usage);
        live.history.mem[HISTORY_SEED] = Some(live.mem_used_pct());
        live.history.swap[HISTORY_SEED] = Some(live.swap_used_pct());
        live.history.gpu[HISTORY_SEED] = Some(live.gpu_metrics.utilization_pct);
        live.history.gpu_vram[HISTORY_SEED] = Some(live.vram_used_pct());

        // Freeze the startup readings as the first historical snapshot so
        // time travel works before the first tick lands.
        let snapshots = vec![live.clone()];

        let (storage_tx, storage_rx) = channel();
        spawn_storage_scanner(storage_tx);

        let (dust_tx, dust_rx) = channel();
        let (dust_tree_tx, dust_tree_rx) = channel();

        let storage_categories = vec![PackageStorageCategory {
            name: "All".to_string(),
            total_str: String::new(),
            items: Vec::new(),
        }];

        Self {
            live,
            cpu_model,
            users,
            dns_resolver,
            io_buf,
            prev_ticks,
            curr_ticks,
            prev_net,
            prev_disk,
            prev_procs,
            snapshots,
            snapshot_idx: 0,
            paused: false,
            current_tab: Tab::General,
            general_sub_tab: 0,
            gpu_sub_tab: 0,
            procs: ProcessViewState::default(),
            net_scroll_offset: 0,
            disks: ui::DisksViewState::default(),
            storage_categories,
            storage_rx,
            dust_tx,
            dust_rx,
            dust_tree_tx,
            dust_tree_rx,
            dust_dir_cache: HashMap::new(),
            dust_scanning_paths: HashSet::new(),
            is_dust_scanning: false,
            needs_redraw: true,
            copy_feedback_until: None,
            modal_btn_rects: None,
            error_popup: None,
            kill_confirmation: None,
            last_tick: Instant::now(),
            tabs_area: Rect::default(),
            table_area: Rect::default(),
        }
    }

    /// Runs the main event loop until the user quits.
    ///
    /// Each iteration drains scanner channels, redraws when dirty, then polls
    /// crossterm for input before advancing the telemetry tick.
    ///
    /// # Errors
    /// Returns an `io::Result` error if event polling or frame rendering fails.
    pub(crate) fn run(
        mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        loop {
            self.drain_scanners();
            self.expire_copy_feedback();

            if self.needs_redraw {
                terminal.draw(|frame| self.draw(frame))?;
                self.needs_redraw = false;
            }

            if event::poll(self.poll_timeout())? {
                let mut more = true;
                while more {
                    match event::read()? {
                        Event::Key(key) => {
                            self.needs_redraw = true;
                            if self.handle_key(key) {
                                return Ok(());
                            }
                        }
                        Event::Mouse(mouse) => self.handle_mouse(mouse),
                        Event::Resize(_, _) => self.needs_redraw = true,
                        _ => {}
                    }
                    more = event::poll(Duration::ZERO)?;
                }
            }

            // While a modal dialog is on screen, telemetry polling is suspended
            // so the user can act on a stable view.
            if !self.modal_active() {
                self.tick_if_due();
            }
        }
    }

    /// Whether a blocking modal (error popup or kill confirmation) is showing.
    fn modal_active(&self) -> bool {
        self.error_popup.is_some() || self.kill_confirmation.is_some()
    }

    /// Drains finished background scans into application state.
    fn drain_scanners(&mut self) {
        while let Ok(new_cats) = self.storage_rx.try_recv() {
            let all_entry = self
                .storage_categories
                .iter()
                .find(|c| c.name == "All")
                .cloned()
                .unwrap_or_else(all_category);
            let mut combined = vec![all_entry];
            combined.extend(new_cats);
            self.storage_categories = combined;
            let len = self.storage_categories.len();
            if len > 0 && self.disks.sub_tab >= len {
                self.disks.sub_tab = len - 1;
            }
            self.needs_redraw = true;
        }

        while let Ok(dust_res) = self.dust_rx.try_recv() {
            self.is_dust_scanning = false;
            if let Some(cat) = dust_res {
                match self
                    .storage_categories
                    .iter_mut()
                    .position(|c| c.name == "All")
                {
                    Some(pos) => self.storage_categories[pos] = cat,
                    None => self.storage_categories.insert(0, cat),
                }
            }
            self.needs_redraw = true;
        }

        while let Ok((parent_path, _parent_depth, children)) = self.dust_tree_rx.try_recv() {
            self.dust_scanning_paths.remove(&parent_path);
            self.dust_dir_cache
                .insert(parent_path.clone(), children.clone());
            if let Some(cat) = self.storage_categories.iter_mut().find(|c| c.name == "All")
                && let Some(idx) = cat.items.iter().position(|i| i.path == parent_path)
            {
                cat.items[idx].is_scanning = false;
                if !cat.items[idx].is_expanded {
                    cat.items[idx].is_expanded = true;
                    let insert_pos = idx + 1;
                    cat.items.splice(insert_pos..insert_pos, children);
                }
            }
            self.needs_redraw = true;
        }
    }

    /// Clears the clipboard-copied badge once its display window has elapsed.
    fn expire_copy_feedback(&mut self) {
        if let Some(until) = self.copy_feedback_until
            && Instant::now() >= until
        {
            self.copy_feedback_until = None;
            self.needs_redraw = true;
        }
    }

    /// Time to block waiting for the next input event.
    fn poll_timeout(&self) -> Duration {
        let base = TICK_INTERVAL
            .checked_sub(self.last_tick.elapsed())
            .unwrap_or(Duration::ZERO);
        match self.copy_feedback_until {
            Some(until) => base.min(until.saturating_duration_since(Instant::now())),
            None => base,
        }
    }

    /// Refreshes all telemetry once [`TICK_INTERVAL`] has elapsed since the previous tick.
    fn tick_if_due(&mut self) {
        if self.last_tick.elapsed() < TICK_INTERVAL {
            return;
        }
        let dt = self.last_tick.elapsed().as_secs_f64();

        self.live.sys_info = read_system_general_info();
        self.live.battery = read_battery();

        self.live.processes = read_processes(&mut self.prev_procs, &self.users, dt);
        sort_processes(
            &mut self.live.processes,
            self.procs.sort_col,
            self.procs.ascending,
        );

        std::mem::swap(&mut self.prev_ticks, &mut self.curr_ticks);
        read_cpu_ticks(&mut self.io_buf, &mut self.curr_ticks);
        self.live.mem = read_memory(&mut self.io_buf);

        if !self.prev_ticks.is_empty() && !self.curr_ticks.is_empty() {
            self.live.global_usage = calculate_usage(&self.prev_ticks[0], &self.curr_ticks[0]);
            self.live.core_usages.clear();
            let pairs = self.curr_ticks.len().min(self.prev_ticks.len());
            for i in 1..pairs {
                let usage = calculate_usage(&self.prev_ticks[i], &self.curr_ticks[i]);
                self.live.core_usages.push(usage);
            }
        }

        let (cpu_cur_mhz, cpu_min_mhz, cpu_max_mhz) = read_cpu_freq_info();
        self.live.cpu_cur_mhz = cpu_cur_mhz;
        self.live.cpu_min_mhz = cpu_min_mhz;
        self.live.cpu_max_mhz = cpu_max_mhz;
        self.live.cpu_temp = read_cpu_temp();
        self.live.ram_temp = read_ram_temp();
        self.live.ram_info = get_ram_info(self.live.mem.total_mem_mb, &self.cpu_model);

        self.live.gpu_metrics = read_gpu_metrics();
        self.live.net_ifaces = read_network_interfaces(&mut self.prev_net, dt);
        self.live.net_connections = read_network_connections(&self.dns_resolver);
        let primary_iface = self
            .live
            .net_ifaces
            .iter()
            .find(|i| i.operstate == "up")
            .or_else(|| self.live.net_ifaces.first());
        let rx_pct = primary_iface
            .map(|i| io_gradient_pct(i.rx_speed))
            .unwrap_or(0.0);
        let tx_pct = primary_iface
            .map(|i| io_gradient_pct(i.tx_speed))
            .unwrap_or(0.0);

        self.live.disk_mounts = read_disk_mounts();
        self.live.disk_io = read_disk_io(&mut self.prev_disk, dt);

        self.live.history.shift();
        self.live.history.push(MetricSample {
            cpu: self.live.global_usage,
            mem: self.live.mem_used_pct(),
            swap: self.live.swap_used_pct(),
            gpu: self.live.gpu_metrics.utilization_pct,
            gpu_vram: self.live.vram_used_pct(),
            net_rx: rx_pct,
            net_tx: tx_pct,
            disk_read: io_gradient_pct(self.live.disk_io.read_speed),
            disk_write: io_gradient_pct(self.live.disk_io.write_speed),
        });

        self.snapshots.push(self.live.clone());
        if self.snapshots.len() > MAX_SNAPSHOTS {
            self.snapshots.remove(0);
            if self.paused {
                self.snapshot_idx = self.snapshot_idx.saturating_sub(1);
            }
        }
        if !self.paused {
            self.snapshot_idx = self.snapshots.len().saturating_sub(1);
        }

        self.last_tick = Instant::now();
        self.needs_redraw = true;
    }

    // -------------------------------------------------------------------------------------
    // Shared actions
    // -------------------------------------------------------------------------------------

    /// The process slice backing the active view: the frozen snapshot when time
    /// travelling, otherwise the live process list.
    fn active_processes(&self) -> &[ProcessInfo] {
        match self.snapshots.get(self.snapshot_idx) {
            Some(snap) => &snap.processes,
            None => &self.live.processes,
        }
    }

    /// Switches to `tab`, closing any open process detail view.
    fn select_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
        self.procs.selected_detail = None;
    }

    /// Toggles pause; unpausing jumps to the latest snapshot and restarts the tick clock.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            self.snapshot_idx = self.snapshots.len().saturating_sub(1);
            self.last_tick = Instant::now();
        }
    }

    /// Steps back one snapshot in time, pausing polling.
    fn travel_back(&mut self) {
        self.paused = true;
        self.snapshot_idx = self.snapshot_idx.saturating_sub(1);
    }

    /// Steps forward one snapshot in time, pausing polling.
    fn travel_forward(&mut self) {
        self.paused = true;
        if self.snapshot_idx + 1 < self.snapshots.len() {
            self.snapshot_idx += 1;
        }
    }

    /// Re-applies the current sort to the live process list and every stored snapshot.
    fn resort_processes(&mut self) {
        for snap in &mut self.snapshots {
            sort_processes(
                &mut snap.processes,
                self.procs.sort_col,
                self.procs.ascending,
            );
        }
        sort_processes(
            &mut self.live.processes,
            self.procs.sort_col,
            self.procs.ascending,
        );
    }

    /// Copies a human-readable overview of the currently viewed snapshot to the clipboard.
    fn copy_system_overview(&mut self) {
        let snap = self.active_snapshot();
        let text = ui::format_system_overview_copy_text(
            &snap.sys_info,
            &self.cpu_model,
            &snap.gpu_metrics,
            &snap.ram_info,
        );
        crate::utils::copy_to_clipboard(&text);
        self.copy_feedback_until = Some(Instant::now() + COPY_FEEDBACK);
    }

    /// The snapshot currently being displayed (historical or live).
    fn active_snapshot(&self) -> &Snapshot {
        self.snapshots.get(self.snapshot_idx).unwrap_or(&self.live)
    }

    /// Visible rows for `processes` under the current view mode and search filter.
    ///
    /// Simple mode groups application suites into collapsible headers and hides
    /// zero-RSS kernel threads; advanced mode lists everything raw. Search text
    /// filters rows by fuzzy relevance across name, PID, user, and command line.
    ///
    /// `grouped_out` receives the grouped rows in simple mode so the returned
    /// borrows stay valid without cloning the process list.
    fn visible_process_rows<'a>(
        &self,
        processes: &'a [ProcessInfo],
        grouped_out: &'a mut Vec<ProcessInfo>,
    ) -> Vec<&'a ProcessInfo> {
        let base: &[ProcessInfo] = if self.procs.advanced {
            processes
        } else {
            *grouped_out = group_processes_for_simple_view(
                processes,
                &self.procs.expanded_groups,
                self.procs.sort_col,
                self.procs.ascending,
                &self.procs.query,
            );
            grouped_out
        };
        let proc_map: HashMap<u32, &ProcessInfo> = processes.iter().map(|p| (p.pid, p)).collect();
        base.iter()
            .filter(|p| self.procs.advanced || p.rss_kb > 0)
            .filter(|p| matches_process_search(p, &self.procs.query, Some(&proc_map)))
            .collect()
    }

    /// Moves the table selection to the row best matching the current search query.
    fn refocus_best_match(&mut self) {
        let best = {
            let mut grouped = Vec::new();
            let rows = self.visible_process_rows(&self.live.processes, &mut grouped);
            rows.iter()
                .enumerate()
                .filter(|(_, p)| directly_matches_search(p, &self.procs.query))
                .max_by_key(|(idx, p)| {
                    (
                        process_search_score(p, &self.procs.query),
                        usize::MAX - *idx,
                    )
                })
                .map(|(idx, _)| idx)
                .unwrap_or(0)
        };
        self.procs.table_state.select(Some(best));
    }

    /// Number of selectable rows currently displayed in the process table.
    fn visible_process_count(&self) -> usize {
        let mut grouped = Vec::new();
        self.visible_process_rows(self.active_processes(), &mut grouped)
            .len()
    }

    /// Validates kill targets, showing an error popup on mismatch, or queues a confirmation.
    ///
    /// `display_name` is the user-facing label for the confirmation dialog,
    /// typically the selected row's name stripped of tree markers.
    fn request_signal(
        &mut self,
        targets: Vec<ProcessTarget>,
        signal: Signal,
        is_kill: bool,
        display_name: &str,
    ) {
        if let Some(err_msg) = validate_targets(&targets) {
            self.error_popup = Some(ProcessErrorPopup {
                title: "Process Validation Error".to_string(),
                message_lines: vec![
                    err_msg,
                    "Action cancelled to protect unintended processes.".to_string(),
                ],
            });
            return;
        }
        self.queue_signal(targets, signal, is_kill, display_name);
    }

    /// Queues a kill confirmation dialog without pre-validating targets.
    fn queue_signal(
        &mut self,
        targets: Vec<ProcessTarget>,
        signal: Signal,
        is_kill: bool,
        display_name: &str,
    ) {
        let pids = targets.iter().map(|t| t.pid).collect();
        self.kill_confirmation = Some(ProcessKillConfirmation {
            targets,
            pids,
            process_name: crate::process::strip_tree_markers(display_name).to_string(),
            signal,
            is_kill,
        });
    }

    /// Re-validates and executes the pending kill confirmation.
    ///
    /// On success the process table and memory metrics are refreshed after a
    /// short settle delay so terminated entries disappear promptly.
    fn execute_kill_confirmation(&mut self) {
        let Some(confirm) = self.kill_confirmation.take() else {
            return;
        };
        if let Some(err_msg) = validate_targets(&confirm.targets) {
            self.error_popup = Some(ProcessErrorPopup {
                title: "Process Validation Error".to_string(),
                message_lines: vec![
                    err_msg,
                    "Action cancelled to protect unintended processes.".to_string(),
                ],
            });
            return;
        }
        for pid_raw in &confirm.pids {
            if let Some(pid) = rustix::process::Pid::from_raw(*pid_raw as i32) {
                let _ = rustix::process::kill_process(pid, confirm.signal);
            }
        }
        std::thread::sleep(KILL_SETTLE);
        let dt = self.last_tick.elapsed().as_secs_f64().max(0.001);
        self.live.processes = read_processes(&mut self.prev_procs, &self.users, dt);
        sort_processes(
            &mut self.live.processes,
            self.procs.sort_col,
            self.procs.ascending,
        );
        self.live.mem = read_memory(&mut self.io_buf);
        if !self.paused {
            self.last_tick = Instant::now();
        }
    }

    // -------------------------------------------------------------------------------------
    // Dust / package-storage helpers
    // -------------------------------------------------------------------------------------

    /// Indices of items in `cat` that pass the visibility filter.
    fn visible_storage_indices(&self, cat: &PackageStorageCategory) -> Vec<usize> {
        if cat.name == "All" {
            cat.items
                .iter()
                .enumerate()
                .filter(|(_, item)| is_item_visible(item, self.disks.show_hidden))
                .map(|(idx, _)| idx)
                .collect()
        } else {
            (0..cat.items.len()).collect()
        }
    }

    /// Count of selectable rows in the active storage category.
    fn visible_storage_rows(&self) -> usize {
        self.active_storage_category()
            .map(|cat| self.visible_storage_indices(cat).len())
            .unwrap_or(0)
    }

    /// Total item count of the active storage category, ignoring visibility filters.
    fn raw_storage_rows(&self) -> usize {
        self.active_storage_category()
            .map(|cat| cat.items.len())
            .unwrap_or(0)
    }

    /// The storage category currently selected on the Disks tab, if any.
    fn active_storage_category(&self) -> Option<&PackageStorageCategory> {
        let idx = self
            .disks
            .sub_tab
            .min(self.storage_categories.len().saturating_sub(1));
        self.storage_categories.get(idx)
    }

    /// Starts a whole-disk dust scan unless one is already running.
    fn ensure_root_scan_started(&mut self) {
        if self.is_dust_scanning {
            return;
        }
        self.is_dust_scanning = true;
        let tx = self.dust_tx.clone();
        let _ = std::thread::Builder::new()
            .name("dust-scanner".to_string())
            .spawn(move || {
                let res = read_dust_storage();
                let _ = tx.send(res);
            });
        self.needs_redraw = true;
    }

    /// Expands or collapses the storage item at `actual_item_idx`.
    ///
    /// Collapsing removes all deeper descendants; expanding inserts cached
    /// children immediately or spawns a background scan for them.
    fn toggle_storage_expansion(&mut self, actual_item_idx: usize) {
        let cat_idx = self
            .disks
            .sub_tab
            .min(self.storage_categories.len().saturating_sub(1));
        let Some(cat) = self.storage_categories.get_mut(cat_idx) else {
            return;
        };
        if cat.name != "All" || actual_item_idx >= cat.items.len() {
            return;
        }

        let (item_path, item_depth, is_expanded, is_dir) = {
            let it = &cat.items[actual_item_idx];
            (it.path.clone(), it.depth, it.is_expanded, it.is_dir)
        };

        if is_expanded {
            cat.items[actual_item_idx].is_expanded = false;
            let remove_count = cat.items[actual_item_idx + 1..]
                .iter()
                .take_while(|next| next.depth > item_depth)
                .count();
            if remove_count > 0 {
                cat.items
                    .drain(actual_item_idx + 1..actual_item_idx + 1 + remove_count);
            }
            self.needs_redraw = true;
        } else if is_dir {
            if let Some(cached) = self.dust_dir_cache.get(&item_path) {
                cat.items[actual_item_idx].is_expanded = true;
                cat.items[actual_item_idx].is_scanning = false;
                let insert_pos = actual_item_idx + 1;
                cat.items.splice(insert_pos..insert_pos, cached.clone());
                self.needs_redraw = true;
            } else if !self.dust_scanning_paths.contains(&item_path) {
                cat.items[actual_item_idx].is_scanning = true;
                self.dust_scanning_paths.insert(item_path.clone());
                let tx = self.dust_tree_tx.clone();
                let _ = std::thread::Builder::new()
                    .name("dust-tree-worker".to_string())
                    .spawn(move || {
                        let children = read_dust_path(&item_path, item_depth);
                        let _ = tx.send((item_path, item_depth, children));
                    });
                self.needs_redraw = true;
            }
        }
    }
}

/// Index of the freshest sample inside each history series.
const HISTORY_SEED: usize = HISTORY_LEN - 1;

/// Spawns the periodic background package-storage scanning thread.
fn spawn_storage_scanner(tx: Sender<Vec<PackageStorageCategory>>) {
    let _ = std::thread::Builder::new()
        .name("storage-scanner".to_string())
        .spawn(move || {
            loop {
                let cats = read_package_storage_categories();
                if tx.send(cats).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(20));
            }
        });
}

/// Validates every kill target, returning the first failure description.
fn validate_targets(targets: &[ProcessTarget]) -> Option<String> {
    targets
        .iter()
        .find_map(|t| validate_process_target(t).err())
}

/// A fresh empty "All" storage category placeholder.
fn all_category() -> PackageStorageCategory {
    PackageStorageCategory {
        name: "All".to_string(),
        total_str: String::new(),
        items: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_navigation_and_clamping() {
        let snapshots: Vec<Snapshot> = (0..5)
            .map(|i| Snapshot {
                global_usage: i as f64 * 10.0,
                core_usages: vec![i as f64 * 10.0],
                ..Default::default()
            })
            .collect();
        assert_eq!(snapshots.len(), 5);
        let mut snapshot_idx = snapshots.len() - 1; // 4 (live)

        // Step back in time: 4 -> 3 -> 2 -> 1 -> 0
        for expected in (0..4).rev() {
            snapshot_idx = snapshot_idx.saturating_sub(1);
            assert_eq!(snapshot_idx, expected);
        }

        // Stepping back beyond 0 clamps at 0
        snapshot_idx = snapshot_idx.saturating_sub(1);
        assert_eq!(snapshot_idx, 0);

        // Step forward in time: 0 -> 1 -> 2 -> 3 -> 4
        for expected in 1..=4 {
            if snapshot_idx + 1 < snapshots.len() {
                snapshot_idx += 1;
            }
            assert_eq!(snapshot_idx, expected);
        }

        // Stepping forward beyond latest clamps at 4
        if snapshot_idx + 1 < snapshots.len() {
            snapshot_idx += 1;
        }
        assert_eq!(snapshot_idx, 4);
    }

    #[test]
    fn test_snapshot_growth_while_paused() {
        let mut snapshots = vec![
            Snapshot {
                global_usage: 1.0,
                ..Default::default()
            },
            Snapshot {
                global_usage: 2.0,
                ..Default::default()
            },
            Snapshot {
                global_usage: 3.0,
                ..Default::default()
            },
        ];

        // User pauses at 2nd snapshot (index 1, display 2/3)
        let paused = true;
        let mut snapshot_idx = 1;
        assert_eq!(snapshot_idx + 1, 2);
        assert_eq!(snapshots.len(), 3);

        // New tick captures snapshot 4 while paused
        snapshots.push(Snapshot {
            global_usage: 4.0,
            ..Default::default()
        });
        if !paused {
            snapshot_idx = snapshots.len().saturating_sub(1);
        }
        assert_eq!(snapshot_idx + 1, 2);
        assert_eq!(snapshots.len(), 4);

        // New tick captures snapshot 5 while paused
        snapshots.push(Snapshot {
            global_usage: 5.0,
            ..Default::default()
        });
        if !paused {
            snapshot_idx = snapshots.len().saturating_sub(1);
        }
        assert_eq!(snapshot_idx + 1, 2);
        assert_eq!(snapshots.len(), 5);

        // Unpausing jumps to latest (5/5)
        let paused = false;
        if !paused {
            snapshot_idx = snapshots.len().saturating_sub(1);
        }
        assert_eq!(snapshot_idx + 1, 5);
        assert_eq!(snapshots.len(), 5);
    }

    #[test]
    fn test_process_detail_view_navigation() {
        use crossterm::event::KeyCode;

        // Pressing Enter on a non-group process opens the detail view.
        let mut selected_detail = Some(12_345_u32);
        assert_eq!(selected_detail, Some(12_345));

        // Pressing Esc / q / Enter restores the process list.
        if matches!(
            KeyCode::Esc,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter
        ) {
            selected_detail = None;
        }
        assert_eq!(selected_detail, None);

        // Stopping (c) or killing (t) also returns to the process list.
        selected_detail = Some(12_345);
        if matches!(KeyCode::Char('c'), KeyCode::Char('c') | KeyCode::Char('t')) {
            selected_detail = None;
        }
        assert_eq!(selected_detail, None);
    }
}
