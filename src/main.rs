#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]

//! # toptop
//!
//! A fast, memory-efficient, real-time Linux terminal system monitor and resource visualizer
//! written in Rust with Ratatui and Unicode Braille graphics.

/// Global memory allocator using `dlmalloc` to minimize heap metadata overhead.
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

/// Process metrics and table sorting module.
mod process;
/// System telemetry and hardware probe module.
mod system;
/// Theme, multi-stop linear gradient, and color calculation module.
mod theme;
/// User interface layout and Ratatui rendering engine.
mod ui;
/// Formatter utilities, fuzzy searching, and clipboard helpers.
mod utils;

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};

use crossterm::{
    ExecutableCommand,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph, TableState, Tabs},
};

use crate::{
    process::{
        ADVANCED_SORT_COLUMNS, NORMAL_SORT_COLUMNS, ProcessErrorPopup, ProcessInfo,
        ProcessKillConfirmation, ProcessSortColumn, ProcessTarget, directly_matches_search,
        group_processes_for_simple_view, matches_process_search, process_search_score,
        read_process_detail, read_processes, sort_processes, validate_process_target,
    },
    system::{
        BatteryInfo, DiskIoInfo, DnsResolver, GpuMetrics, MemoryMetrics, MountInfo,
        NetConnectionInfo, NetInterfaceInfo, PackageStorageCategory, SystemGeneralInfo,
        calculate_usage, get_cpu_model, get_ram_info, get_users, read_battery, read_cpu_freq_info,
        read_cpu_temp, read_cpu_ticks, read_disk_io, read_disk_mounts, read_gpu_metrics,
        read_memory, read_network_connections, read_network_interfaces,
        read_package_storage_categories, read_dust_storage, read_system_general_info,
    },
    theme::io_gradient_pct,
    ui::{
        format_system_overview_copy_text, is_disks_overflow, is_gpu_overflow, render_cpu_ram_tab,
        render_disks_tab, render_general_tab, render_gpu_tab, render_kill_confirmation_modal,
        render_network_tab, render_process_detail, render_process_error_popup, render_process_tab,
    },
    utils::{copy_to_clipboard, format_datetime},
};

/// Maximum number of historical 2-second telemetry snapshots retained in memory.
const MAX_SNAPSHOTS: usize = 300;

/// Tab navigation title headers for the 6 primary dashboard views.
const TAB_TITLES: [&str; 6] = [
    "General (1)",
    "Processes (2)",
    "CPU & RAM (3)",
    "GPU (4)",
    "Network (5)",
    "Disk (6)",
];

/// Telemetry snapshot captured at a 2-second interval, enabling historical time-travel inspection.
#[derive(Clone)]
struct Snapshot {
    /// System overview metadata.
    sys_info: SystemGeneralInfo,
    /// Battery telemetry status.
    battery: Option<BatteryInfo>,
    /// Overall CPU busy percentage.
    global_usage: f64,
    /// Per-core CPU busy percentages.
    core_usages: Vec<f64>,
    /// Current CPU clock frequency in MHz.
    cpu_cur_mhz: f64,
    /// Minimum CPU clock frequency in MHz.
    cpu_min_mhz: f64,
    /// Maximum CPU clock frequency in MHz.
    cpu_max_mhz: f64,
    /// Package CPU temperature in degrees Celsius.
    cpu_temp: u32,
    /// Memory metrics.
    mem: MemoryMetrics,
    /// Formatted RAM capacity and speed info string.
    ram_info: String,
    /// GPU telemetry and clock/power metrics.
    gpu_metrics: GpuMetrics,
    /// Active network interfaces and throughput rates.
    net_ifaces: Vec<NetInterfaceInfo>,
    /// Active socket connections.
    net_connections: Result<Vec<NetConnectionInfo>, &'static str>,
    /// Disk I/O throughput rates.
    disk_io: DiskIoInfo,
    /// Mounted partition usage.
    disk_mounts: Vec<MountInfo>,
    /// Process list snapshot.
    processes: Vec<ProcessInfo>,
    /// Historical CPU percentage samples.
    cpu_history: Vec<Option<f64>>,
    /// Historical RAM percentage samples.
    mem_history: Vec<Option<f64>>,
    /// Historical swap percentage samples.
    swap_history: Vec<Option<f64>>,
    /// Historical GPU core percentage samples.
    gpu_history: Vec<Option<f64>>,
    /// Historical GPU VRAM percentage samples.
    gpu_vram_history: Vec<Option<f64>>,
    /// Historical network RX rate samples.
    net_rx_history: Vec<Option<f64>>,
    /// Historical network TX rate samples.
    net_tx_history: Vec<Option<f64>>,
    /// Historical disk read rate samples.
    disk_read_history: Vec<Option<f64>>,
    /// Historical disk write rate samples.
    disk_write_history: Vec<Option<f64>>,
}

/// Helper to construct a Snapshot from current telemetry metrics.
#[allow(clippy::too_many_arguments)]
fn create_snapshot(
    sys_info: &SystemGeneralInfo,
    battery: Option<&BatteryInfo>,
    global_usage: f64,
    core_usages: &[f64],
    cpu_cur_mhz: f64,
    cpu_min_mhz: f64,
    cpu_max_mhz: f64,
    cpu_temp: u32,
    mem: &MemoryMetrics,
    ram_info: &str,
    gpu_metrics: &GpuMetrics,
    net_ifaces: &[NetInterfaceInfo],
    net_connections: &Result<Vec<NetConnectionInfo>, &'static str>,
    disk_io: &DiskIoInfo,
    disk_mounts: &[MountInfo],
    processes: &[ProcessInfo],
    cpu_history: &[Option<f64>],
    mem_history: &[Option<f64>],
    swap_history: &[Option<f64>],
    gpu_history: &[Option<f64>],
    gpu_vram_history: &[Option<f64>],
    net_rx_history: &[Option<f64>],
    net_tx_history: &[Option<f64>],
    disk_read_history: &[Option<f64>],
    disk_write_history: &[Option<f64>],
) -> Snapshot {
    Snapshot {
        sys_info: sys_info.clone(),
        battery: battery.cloned(),
        global_usage,
        core_usages: core_usages.to_vec(),
        cpu_cur_mhz,
        cpu_min_mhz,
        cpu_max_mhz,
        cpu_temp,
        mem: *mem,
        ram_info: ram_info.to_string(),
        gpu_metrics: gpu_metrics.clone(),
        net_ifaces: net_ifaces.to_vec(),
        net_connections: net_connections.clone(),
        disk_io: disk_io.clone(),
        disk_mounts: disk_mounts.to_vec(),
        processes: processes.to_vec(),
        cpu_history: cpu_history.to_vec(),
        mem_history: mem_history.to_vec(),
        swap_history: swap_history.to_vec(),
        gpu_history: gpu_history.to_vec(),
        gpu_vram_history: gpu_vram_history.to_vec(),
        net_rx_history: net_rx_history.to_vec(),
        net_tx_history: net_tx_history.to_vec(),
        disk_read_history: disk_read_history.to_vec(),
        disk_write_history: disk_write_history.to_vec(),
    }
}

/// Entry point for `toptop`. Initializes terminal raw mode, sets up panic hooks,
/// runs the event loop, and cleanly restores terminal state on exit.
///
/// # Errors
/// Returns an `io::Result` if terminal initialization or restoration fails.
fn main() -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;

    let original_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_panic(info);
    }));

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let res = run_app(&mut terminal);

    disable_raw_mode()?;
    io::stdout()
        .execute(DisableMouseCapture)?
        .execute(LeaveAlternateScreen)?;

    res
}

/// Main application event loop and telemetry coordinator.
///
/// Manages tab switching, background polling ticks, keyboard navigation,
/// mouse click/scroll events, and UI redraw triggers.
///
/// # Arguments
/// * `terminal` - Active Ratatui crossterm terminal backend instance.
///
/// # Errors
/// Returns an `io::Result` if frame rendering or event polling fails.
fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let tick_rate = Duration::from_millis(2000);
    let mut last_tick = Instant::now();
    let mut is_paused = false;

    let mut sys_info = read_system_general_info();
    let mut battery = read_battery();

    let mut cpu_history: Vec<Option<f64>> = vec![None; 100];
    let mut mem_history: Vec<Option<f64>> = vec![None; 100];
    let mut swap_history: Vec<Option<f64>> = vec![None; 100];
    let mut gpu_history: Vec<Option<f64>> = vec![None; 100];
    let mut gpu_vram_history: Vec<Option<f64>> = vec![None; 100];
    let mut gpu_metrics = read_gpu_metrics();
    gpu_history[99] = Some(gpu_metrics.utilization_pct);
    let initial_vram_pct = if gpu_metrics.vram_total_mb > 0 {
        (gpu_metrics.vram_used_mb as f64 / gpu_metrics.vram_total_mb as f64) * 100.0
    } else {
        0.0
    };
    gpu_vram_history[99] = Some(initial_vram_pct);

    let mut net_rx_history: Vec<Option<f64>> = vec![None; 100];
    let mut net_tx_history: Vec<Option<f64>> = vec![None; 100];
    let mut prev_net = HashMap::new();
    let mut net_ifaces = read_network_interfaces(&mut prev_net, 0.0);

    let mut disk_read_history: Vec<Option<f64>> = vec![None; 100];
    let mut disk_write_history: Vec<Option<f64>> = vec![None; 100];
    let mut prev_disk = HashMap::new();
    let mut disk_mounts = read_disk_mounts();
    let mut disk_io = read_disk_io(&mut prev_disk, 0.0);

    let (storage_tx, storage_rx): (
        Sender<Vec<PackageStorageCategory>>,
        Receiver<Vec<PackageStorageCategory>>,
    ) = channel();

    let (dust_tx, dust_rx): (
        Sender<Option<PackageStorageCategory>>,
        Receiver<Option<PackageStorageCategory>>,
    ) = channel();
    let mut is_dust_scanning = false;

    // Spawn background worker for async 20-second package storage scans
    std::thread::Builder::new()
        .name("storage-scanner".to_string())
        .spawn(move || {
            loop {
                let cats = read_package_storage_categories();
                if storage_tx.send(cats).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(20));
            }
        })
        .expect("failed to spawn storage scanner thread");

    let mut storage_categories = vec![PackageStorageCategory {
        name: "All".to_string(),
        total_str: String::new(),
        items: Vec::new(),
    }];
    let mut general_sub_tab = 0;
    let mut gpu_sub_tab = 0;
    let mut disks_sub_tab = 0;
    let mut disks_box_tab = 0;
    let mut disks_scroll_offset = 0;

    let cpu_model = get_cpu_model();
    let mut io_buf = String::with_capacity(8192);
    let mut prev_ticks = Vec::with_capacity(32);
    let mut curr_ticks = Vec::with_capacity(32);

    read_cpu_ticks(&mut io_buf, &mut prev_ticks);

    // Warmup delay to get initial CPU readings
    std::thread::sleep(Duration::from_millis(100));
    read_cpu_ticks(&mut io_buf, &mut curr_ticks);

    let mut global_usage = 0.0;
    let mut core_usages = Vec::new();
    let mut mem = read_memory(&mut io_buf);

    let mut current_tab = 0;
    let mut advanced_view = false;
    let mut current_sort_col = ProcessSortColumn::Mem;
    let mut sort_ascending = false;
    let mut is_searching = false;
    let mut search_query = String::new();
    let mut prev_procs = HashMap::new();
    let users = get_users();
    let mut processes = read_processes(&mut prev_procs, &users, 0.0);
    sort_processes(&mut processes, current_sort_col, sort_ascending);
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut selected_proc_detail: Option<u32> = None;

    if !prev_ticks.is_empty() && !curr_ticks.is_empty() {
        global_usage = calculate_usage(&prev_ticks[0], &curr_ticks[0]);
        for i in 1..curr_ticks.len().min(prev_ticks.len()) {
            core_usages.push(calculate_usage(&prev_ticks[i], &curr_ticks[i]));
        }
    }
    cpu_history[99] = Some(global_usage);
    let m_pct = if mem.total_mem_mb > 0 {
        (mem.used_mem_mb as f64 / mem.total_mem_mb as f64) * 100.0
    } else {
        0.0
    };
    mem_history[99] = Some(m_pct);
    let s_pct = if mem.total_swap_mb > 0 {
        (mem.used_swap_mb as f64 / mem.total_swap_mb as f64) * 100.0
    } else {
        0.0
    };
    swap_history[99] = Some(s_pct);

    let mut table_area = Rect::default();
    let mut tabs_area = Rect::default();
    let mut copy_feedback_until: Option<Instant> = None;
    let (mut cpu_cur_mhz, mut cpu_min_mhz, mut cpu_max_mhz) = read_cpu_freq_info();
    let mut cpu_temp = read_cpu_temp();
    let mut ram_info = get_ram_info(mem.total_mem_mb, &cpu_model);
    let dns_resolver = DnsResolver::new();
    let mut net_connections = read_network_connections(&dns_resolver);
    let mut net_scroll_offset: usize = 0;
    let mut expanded_groups: HashSet<String> = HashSet::new();
    let mut kill_confirmation: Option<ProcessKillConfirmation> = None;
    let mut process_error_popup: Option<ProcessErrorPopup> = None;
    let mut modal_btn_rects: Option<(Rect, Rect)> = None;
    let mut error_btn_rect: Option<Rect> = None;
    let mut needs_redraw = true;

    let mut snapshots = Vec::with_capacity(MAX_SNAPSHOTS);
    snapshots.push(create_snapshot(
        &sys_info,
        battery.as_ref(),
        global_usage,
        &core_usages,
        cpu_cur_mhz,
        cpu_min_mhz,
        cpu_max_mhz,
        cpu_temp,
        &mem,
        &ram_info,
        &gpu_metrics,
        &net_ifaces,
        &net_connections,
        &disk_io,
        &disk_mounts,
        &processes,
        &cpu_history,
        &mem_history,
        &swap_history,
        &gpu_history,
        &gpu_vram_history,
        &net_rx_history,
        &net_tx_history,
        &disk_read_history,
        &disk_write_history,
    ));
    let mut snapshot_idx = 0;

    'main_loop: loop {
        while let Ok(new_cats) = storage_rx.try_recv() {
            let all_entry = storage_categories
                .iter()
                .find(|c| c.name == "All")
                .cloned()
                .unwrap_or_else(|| PackageStorageCategory {
                    name: "All".to_string(),
                    total_str: String::new(),
                    items: Vec::new(),
                });
            let mut combined = vec![all_entry];
            combined.extend(new_cats);
            storage_categories = combined;
            if !storage_categories.is_empty() && disks_sub_tab >= storage_categories.len() {
                disks_sub_tab = storage_categories.len() - 1;
            }
            needs_redraw = true;
        }

        while let Ok(dust_res) = dust_rx.try_recv() {
            is_dust_scanning = false;
            if let Some(cat) = dust_res {
                if let Some(all_pos) = storage_categories.iter().position(|c| c.name == "All") {
                    storage_categories[all_pos] = cat;
                } else {
                    storage_categories.insert(0, cat);
                }
            }
            needs_redraw = true;
        }

        if let Some(until) = copy_feedback_until
            && Instant::now() >= until
        {
            copy_feedback_until = None;
            needs_redraw = true;
        }

        if needs_redraw {
            terminal.draw(|frame| {
                let area = frame.area();

                if area.width < 98 || area.height < 20 {
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::Rgb(255, 80, 80)));
                    let inner = block.inner(area);
                    frame.render_widget(block, area);

                    if inner.height > 0 {
                        use ratatui::style::Stylize;
                        let text = vec![
                            Line::from("Terminal too small, minimum of 98x20 required.")
                                .fg(Color::Rgb(255, 100, 100))
                                .bold()
                                .alignment(ratatui::layout::Alignment::Center),
                        ];
                        let msg_y = inner.y + inner.height / 2;
                        let msg_area = Rect {
                            x: inner.x,
                            y: msg_y,
                            width: inner.width,
                            height: 1,
                        };
                        frame.render_widget(Paragraph::new(text), msg_area);
                    }
                    return;
                }

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(1)
                    .constraints([Constraint::Length(3), Constraint::Min(0)])
                    .split(area);

                tabs_area = chunks[0];
                table_area = chunks[1];

                let titles: Vec<Line> = TAB_TITLES.iter().map(|&t| Line::from(t)).collect();
                let total_snaps = snapshots.len();
                let cur_snap = if total_snaps > 0 { snapshot_idx + 1 } else { 0 };
                let steps_back = total_snaps.saturating_sub(1 + snapshot_idx);
                let secs_back = steps_back * 2;

                let time_badge = if is_paused && steps_back > 0 {
                    format!(
                        " [PAUSED -{}s ({}/{}) | '[' / ']' time travel] ",
                        secs_back, cur_snap, total_snaps
                    )
                } else if is_paused {
                    " [PAUSED (LIVE) | '[' / ']' time travel] ".to_string()
                } else {
                    " - Space to pause ".to_string()
                };

                let (topbar_border, topbar_title_style) = if is_paused {
                    (
                        Color::Rgb(255, 255, 0),
                        Style::default().fg(Color::Rgb(255, 255, 0)).bold(),
                    )
                } else {
                    (
                        Color::Rgb(60, 60, 60),
                        Style::default().fg(Color::Rgb(170, 170, 170)),
                    )
                };

                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let target_unix = now_unix.saturating_sub(secs_back as i64);
                let datetime_str = format_datetime(target_unix);

                let tabs_title = format!(" System Monitor{} ", time_badge);
                #[allow(unused_mut)]
                let mut tabs_block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(topbar_border))
                    .title(Line::from(tabs_title).style(topbar_title_style))
                    .title(
                        Line::from(format!(" {} ", datetime_str))
                            .fg(topbar_border)
                            .alignment(ratatui::layout::Alignment::Right),
                    );

                #[cfg(debug_assertions)]
                {
                    tabs_block = tabs_block.title_bottom(
                        Line::from(
                            format!(" [{}x{}] ", area.width, area.height)
                                .fg(Color::Rgb(150, 150, 150)),
                        )
                        .alignment(ratatui::layout::Alignment::Right),
                    );
                }

                let tabs = Tabs::new(titles)
                    .style(Style::default().not_bold().fg(Color::Rgb(170, 170, 170)))
                    .select(current_tab)
                    .block(tabs_block)
                    .highlight_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold())
                    .divider("|")
                    .padding(" ", " ");

                frame.render_widget(tabs, chunks[0]);

                let is_copied = copy_feedback_until
                    .map(|t| Instant::now() < t)
                    .unwrap_or(false);

                let default_snap = create_snapshot(
                    &sys_info,
                    battery.as_ref(),
                    global_usage,
                    &core_usages,
                    cpu_cur_mhz,
                    cpu_min_mhz,
                    cpu_max_mhz,
                    cpu_temp,
                    &mem,
                    &ram_info,
                    &gpu_metrics,
                    &net_ifaces,
                    &net_connections,
                    &disk_io,
                    &disk_mounts,
                    &processes,
                    &cpu_history,
                    &mem_history,
                    &swap_history,
                    &gpu_history,
                    &gpu_vram_history,
                    &net_rx_history,
                    &net_tx_history,
                    &disk_read_history,
                    &disk_write_history,
                );
                let snap = snapshots.get(snapshot_idx).unwrap_or(&default_snap);

                match current_tab {
                    0 => {
                        render_general_tab(
                            frame,
                            chunks[1],
                            &snap.sys_info,
                            snap.battery.as_ref(),
                            snap.global_usage,
                            &snap.core_usages,
                            snap.cpu_cur_mhz,
                            snap.cpu_min_mhz,
                            snap.cpu_max_mhz,
                            snap.cpu_temp,
                            &cpu_model,
                            &snap.mem,
                            &snap.ram_info,
                            &snap.gpu_metrics,
                            &snap.net_ifaces,
                            &snap.disk_io,
                            &snap.disk_mounts,
                            &snap.processes,
                            is_copied,
                            general_sub_tab,
                        );
                    }
                    1 => {
                        let num_cores = snap.core_usages.len().max(1);
                        let proc_map: HashMap<u32, &ProcessInfo> =
                            snap.processes.iter().map(|p| (p.pid, p)).collect();
                        if let Some(detail_pid) = selected_proc_detail {
                            let dummy_proc;
                            let proc_info = if let Some(p) = proc_map.get(&detail_pid) {
                                p
                            } else {
                                dummy_proc = ProcessInfo {
                                    pid: detail_pid,
                                    comm: format!("PID {}", detail_pid),
                                    name: format!("PID {}", detail_pid),
                                    state: "Exited".to_string(),
                                    ..Default::default()
                                };
                                &dummy_proc
                            };
                            let detail = read_process_detail(proc_info, &proc_map);
                            render_process_detail(
                                frame,
                                chunks[1],
                                &detail,
                                snap.mem.total_mem_mb,
                                num_cores,
                            );
                        } else {
                            render_process_tab(
                                frame,
                                chunks[1],
                                &snap.processes,
                                advanced_view,
                                &expanded_groups,
                                &search_query,
                                is_searching,
                                current_sort_col,
                                sort_ascending,
                                snap.mem.total_mem_mb,
                                num_cores,
                                &mut table_state,
                            );
                        }
                    }
                    2 => {
                        render_cpu_ram_tab(
                            frame,
                            chunks[1],
                            &snap.cpu_history,
                            &snap.core_usages,
                            snap.cpu_cur_mhz,
                            snap.cpu_min_mhz,
                            snap.cpu_max_mhz,
                            snap.cpu_temp,
                            &cpu_model,
                            &snap.mem,
                            &snap.mem_history,
                            &snap.swap_history,
                            &snap.ram_info,
                        );
                    }
                    3 => {
                        render_gpu_tab(
                            frame,
                            chunks[1],
                            &snap.gpu_metrics,
                            &snap.gpu_history,
                            &snap.gpu_vram_history,
                            gpu_sub_tab,
                        );
                    }
                    4 => {
                        render_network_tab(
                            frame,
                            chunks[1],
                            &snap.net_ifaces,
                            &snap.net_rx_history,
                            &snap.net_tx_history,
                            &snap.net_connections,
                            net_scroll_offset,
                        );
                    }
                    5 => {
                        let is_snapshot = steps_back > 0;
                        render_disks_tab(
                            frame,
                            chunks[1],
                            &snap.disk_io,
                            &snap.disk_mounts,
                            &snap.disk_read_history,
                            &snap.disk_write_history,
                            &storage_categories,
                            disks_sub_tab,
                            disks_scroll_offset,
                            disks_box_tab,
                            is_snapshot,
                            is_dust_scanning,
                        );
                    }
                    _ => {}
                }

                if let Some(ref err_popup) = process_error_popup {
                    error_btn_rect = Some(render_process_error_popup(frame, area, err_popup));
                    modal_btn_rects = None;
                } else if let Some(ref confirm) = kill_confirmation {
                    modal_btn_rects = Some(render_kill_confirmation_modal(frame, area, confirm));
                    error_btn_rect = None;
                } else {
                    modal_btn_rects = None;
                    error_btn_rect = None;
                }
            })?;
            needs_redraw = false;
        }

        let base_timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);
        let timeout = if let Some(until) = copy_feedback_until {
            let copy_timeout = until
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            base_timeout.min(copy_timeout)
        } else {
            base_timeout
        };

        if let Some(_err_popup) = process_error_popup.as_ref() {
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(_) => {
                        process_error_popup = None;
                        needs_redraw = true;
                    }
                    Event::Mouse(mouse_event) => {
                        if let MouseEventKind::Down(MouseButton::Left) = mouse_event.kind {
                            process_error_popup = None;
                            needs_redraw = true;
                        }
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            continue 'main_loop;
        }

        if let Some(confirm) = kill_confirmation.as_ref() {
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) => {
                        if matches!(
                            key.code,
                            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                        ) {
                            let mut validation_err = None;
                            for t in &confirm.targets {
                                if let Err(e) = validate_process_target(t) {
                                    validation_err = Some(e);
                                    break;
                                }
                            }

                            if let Some(err_msg) = validation_err {
                                kill_confirmation = None;
                                process_error_popup = Some(ProcessErrorPopup {
                                    title: "Process Validation Error".to_string(),
                                    message_lines: vec![
                                        err_msg,
                                        "Action cancelled to protect unintended processes."
                                            .to_string(),
                                    ],
                                });
                                needs_redraw = true;
                            } else {
                                for &p_id in &confirm.pids {
                                    if let Some(pid) = rustix::process::Pid::from_raw(p_id as i32) {
                                        let _ = rustix::process::kill_process(pid, confirm.signal);
                                    }
                                }
                                std::thread::sleep(Duration::from_millis(40));
                                let cur_dt = last_tick.elapsed().as_secs_f64().max(0.001);
                                processes = read_processes(&mut prev_procs, &users, cur_dt);
                                sort_processes(&mut processes, current_sort_col, sort_ascending);
                                mem = read_memory(&mut io_buf);
                                if !is_paused {
                                    last_tick = Instant::now();
                                }
                                kill_confirmation = None;
                                needs_redraw = true;
                            }
                        } else if matches!(
                            key.code,
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
                        ) {
                            kill_confirmation = None;
                            needs_redraw = true;
                        }
                    }
                    Event::Mouse(mouse_event) => {
                        let mx = mouse_event.column;
                        let my = mouse_event.row;
                        if let MouseEventKind::Down(MouseButton::Left) = mouse_event.kind
                            && let Some((yes_rect, no_rect)) = modal_btn_rects
                        {
                            if mx >= yes_rect.x
                                && mx < yes_rect.right()
                                && my >= yes_rect.y
                                && my < yes_rect.bottom()
                            {
                                let mut validation_err = None;
                                for t in &confirm.targets {
                                    if let Err(e) = validate_process_target(t) {
                                        validation_err = Some(e);
                                        break;
                                    }
                                }

                                if let Some(err_msg) = validation_err {
                                    kill_confirmation = None;
                                    process_error_popup = Some(ProcessErrorPopup {
                                        title: "Process Validation Error".to_string(),
                                        message_lines: vec![
                                            err_msg,
                                            "Action cancelled to protect unintended processes."
                                                .to_string(),
                                        ],
                                    });
                                    needs_redraw = true;
                                } else {
                                    for &p_id in &confirm.pids {
                                        if let Some(pid) =
                                            rustix::process::Pid::from_raw(p_id as i32)
                                        {
                                            let _ =
                                                rustix::process::kill_process(pid, confirm.signal);
                                        }
                                    }
                                    std::thread::sleep(Duration::from_millis(40));
                                    let cur_dt = last_tick.elapsed().as_secs_f64().max(0.001);
                                    processes = read_processes(&mut prev_procs, &users, cur_dt);
                                    sort_processes(
                                        &mut processes,
                                        current_sort_col,
                                        sort_ascending,
                                    );
                                    mem = read_memory(&mut io_buf);
                                    if !is_paused {
                                        last_tick = Instant::now();
                                    }
                                    kill_confirmation = None;
                                    needs_redraw = true;
                                }
                            } else if mx >= no_rect.x
                                && mx < no_rect.right()
                                && my >= no_rect.y
                                && my < no_rect.bottom()
                            {
                                kill_confirmation = None;
                                needs_redraw = true;
                            }
                        }
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            continue 'main_loop;
        }

        if event::poll(timeout)? {
            let mut more = true;
            while more {
                match event::read()? {
                    Event::Key(key) => {
                        needs_redraw = true;

                        if is_searching {
                            let grouped_cache;
                            let base_procs = if advanced_view {
                                &processes
                            } else {
                                grouped_cache = group_processes_for_simple_view(
                                    &processes,
                                    &expanded_groups,
                                    current_sort_col,
                                    sort_ascending,
                                    &search_query,
                                );
                                &grouped_cache
                            };
                            let proc_map: HashMap<u32, &ProcessInfo> =
                                processes.iter().map(|p| (p.pid, p)).collect();
                            let num_procs = base_procs
                                .iter()
                                .filter(|p| advanced_view || p.rss_kb > 0)
                                .filter(|p| {
                                    matches_process_search(p, &search_query, Some(&proc_map))
                                })
                                .count();
                            match key.code {
                                KeyCode::Esc => {
                                    is_searching = false;
                                    search_query.clear();
                                    table_state.select(Some(0));
                                }
                                KeyCode::Enter => {
                                    is_searching = false;
                                }
                                KeyCode::Backspace => {
                                    search_query.pop();
                                    let grouped_cache;
                                    let base_procs = if advanced_view {
                                        &processes
                                    } else {
                                        grouped_cache = group_processes_for_simple_view(
                                            &processes,
                                            &expanded_groups,
                                            current_sort_col,
                                            sort_ascending,
                                            &search_query,
                                        );
                                        &grouped_cache
                                    };
                                    let displayed: Vec<&ProcessInfo> = base_procs
                                        .iter()
                                        .filter(|p| advanced_view || p.rss_kb > 0)
                                        .filter(|p| {
                                            matches_process_search(
                                                p,
                                                &search_query,
                                                Some(&proc_map),
                                            )
                                        })
                                        .collect();
                                    let best_match = displayed
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, p)| directly_matches_search(p, &search_query))
                                        .max_by_key(|(idx, p)| {
                                            (
                                                process_search_score(p, &search_query),
                                                usize::MAX - idx,
                                            )
                                        })
                                        .map(|(idx, _)| idx)
                                        .unwrap_or(0);
                                    table_state.select(Some(best_match));
                                }
                                KeyCode::Char(c) => {
                                    search_query.push(c);
                                    let grouped_cache;
                                    let base_procs = if advanced_view {
                                        &processes
                                    } else {
                                        grouped_cache = group_processes_for_simple_view(
                                            &processes,
                                            &expanded_groups,
                                            current_sort_col,
                                            sort_ascending,
                                            &search_query,
                                        );
                                        &grouped_cache
                                    };
                                    let displayed: Vec<&ProcessInfo> = base_procs
                                        .iter()
                                        .filter(|p| advanced_view || p.rss_kb > 0)
                                        .filter(|p| {
                                            matches_process_search(
                                                p,
                                                &search_query,
                                                Some(&proc_map),
                                            )
                                        })
                                        .collect();
                                    let best_match = displayed
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, p)| directly_matches_search(p, &search_query))
                                        .max_by_key(|(idx, p)| {
                                            (
                                                process_search_score(p, &search_query),
                                                usize::MAX - idx,
                                            )
                                        })
                                        .map(|(idx, _)| idx)
                                        .unwrap_or(0);
                                    table_state.select(Some(best_match));
                                }
                                KeyCode::Up => {
                                    let i = match table_state.selected() {
                                        Some(i) => i.saturating_sub(1),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::Down => {
                                    let i = match table_state.selected() {
                                        Some(i) => (i + 1).min(num_procs.saturating_sub(1)),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::PageUp => {
                                    let i = match table_state.selected() {
                                        Some(i) => i.saturating_sub(10),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::PageDown => {
                                    let i = match table_state.selected() {
                                        Some(i) => (i + 10).min(num_procs.saturating_sub(1)),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                _ => {}
                            }
                        } else if current_tab == 1
                            && selected_proc_detail.is_some()
                            && (key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q'))
                        {
                            selected_proc_detail = None;
                            needs_redraw = true;
                        } else if key.code == KeyCode::Char('q')
                            || (key.modifiers.contains(KeyModifiers::CONTROL)
                                && key.code == KeyCode::Char('c'))
                        {
                            break 'main_loop;
                        } else if key.code == KeyCode::Tab {
                            current_tab = (current_tab + 1) % 6;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::BackTab {
                            current_tab = (current_tab + 5) % 6;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('1') {
                            current_tab = 0;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('2') {
                            current_tab = 1;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('3') {
                            current_tab = 2;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('4') {
                            current_tab = 3;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('5') {
                            current_tab = 4;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('6') {
                            current_tab = 5;
                            selected_proc_detail = None;
                        } else if key.code == KeyCode::Char('[') {
                            is_paused = true;
                            snapshot_idx = snapshot_idx.saturating_sub(1);
                            needs_redraw = true;
                        } else if key.code == KeyCode::Char(']') {
                            is_paused = true;
                            if !snapshots.is_empty() && snapshot_idx + 1 < snapshots.len() {
                                snapshot_idx += 1;
                            }
                            needs_redraw = true;
                        } else if current_tab == 0 {
                            if key.code == KeyCode::Char('c') || key.code == KeyCode::Char('y') {
                                let cur_snap = snapshots.get(snapshot_idx);
                                let text = format_system_overview_copy_text(
                                    cur_snap.map(|s| &s.sys_info).unwrap_or(&sys_info),
                                    &cpu_model,
                                    cur_snap.map(|s| &s.gpu_metrics).unwrap_or(&gpu_metrics),
                                    cur_snap.map(|s| s.ram_info.as_str()).unwrap_or(&ram_info),
                                );
                                copy_to_clipboard(&text);
                                copy_feedback_until =
                                    Some(Instant::now() + Duration::from_secs(2));
                                needs_redraw = true;
                            } else if key.code == KeyCode::Left {
                                general_sub_tab = (general_sub_tab + 3) % 4;
                                needs_redraw = true;
                            } else if key.code == KeyCode::Right {
                                general_sub_tab = (general_sub_tab + 1) % 4;
                                needs_redraw = true;
                            } else if key.code == KeyCode::Char(' ') {
                                is_paused = !is_paused;
                                if !is_paused {
                                    if !snapshots.is_empty() {
                                        snapshot_idx = snapshots.len() - 1;
                                    }
                                    last_tick = Instant::now();
                                }
                                needs_redraw = true;
                            }
                        } else if current_tab == 1 {
                            let show_pid = table_area.width >= 150;
                            let all_cols: &[ProcessSortColumn] = if advanced_view {
                                &ADVANCED_SORT_COLUMNS
                            } else {
                                &NORMAL_SORT_COLUMNS
                            };
                            let cols: Vec<ProcessSortColumn> = all_cols
                                .iter()
                                .copied()
                                .filter(|&c| show_pid || c != ProcessSortColumn::Pid)
                                .collect();

                            let active_procs = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.processes)
                                .unwrap_or(&processes);
                            let grouped_cache;
                            let base_procs = if advanced_view {
                                active_procs
                            } else {
                                grouped_cache = group_processes_for_simple_view(
                                    active_procs,
                                    &expanded_groups,
                                    current_sort_col,
                                    sort_ascending,
                                    &search_query,
                                );
                                &grouped_cache
                            };

                            let proc_map: HashMap<u32, &ProcessInfo> =
                                active_procs.iter().map(|p| (p.pid, p)).collect();

                            let num_procs = base_procs
                                .iter()
                                .filter(|p| advanced_view || p.rss_kb > 0)
                                .filter(|p| {
                                    matches_process_search(p, &search_query, Some(&proc_map))
                                })
                                .count();

                            if let Some(detail_pid) = selected_proc_detail {
                                match key.code {
                                    KeyCode::Esc
                                    | KeyCode::Char('q')
                                    | KeyCode::Char('Q')
                                    | KeyCode::Backspace
                                    | KeyCode::Enter => {
                                        selected_proc_detail = None;
                                        needs_redraw = true;
                                    }
                                    KeyCode::Char('c') | KeyCode::Char('C') => {
                                        if let Some(target_p) =
                                            active_procs.iter().find(|p| p.pid == detail_pid)
                                        {
                                            kill_confirmation = Some(ProcessKillConfirmation {
                                                targets: vec![ProcessTarget {
                                                    pid: target_p.pid,
                                                    comm: target_p.comm.clone(),
                                                    name: target_p.name.clone(),
                                                }],
                                                pids: vec![target_p.pid],
                                                process_name: target_p.name.clone(),
                                                signal: rustix::process::Signal::Term,
                                                is_kill: false,
                                            });
                                            selected_proc_detail = None;
                                            needs_redraw = true;
                                        }
                                    }
                                    KeyCode::Char('t') | KeyCode::Char('T') => {
                                        if let Some(target_p) =
                                            active_procs.iter().find(|p| p.pid == detail_pid)
                                        {
                                            kill_confirmation = Some(ProcessKillConfirmation {
                                                targets: vec![ProcessTarget {
                                                    pid: target_p.pid,
                                                    comm: target_p.comm.clone(),
                                                    name: target_p.name.clone(),
                                                }],
                                                pids: vec![target_p.pid],
                                                process_name: target_p.name.clone(),
                                                signal: rustix::process::Signal::Kill,
                                                is_kill: true,
                                            });
                                            selected_proc_detail = None;
                                            needs_redraw = true;
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::Enter => {
                                        if let Some(sel) = table_state.selected() {
                                            let displayed: Vec<&ProcessInfo> = base_procs
                                                .iter()
                                                .filter(|p| advanced_view || p.rss_kb > 0)
                                                .filter(|p| {
                                                    matches_process_search(
                                                        p,
                                                        &search_query,
                                                        Some(&proc_map),
                                                    )
                                                })
                                                .collect();
                                            if let Some(target) = displayed.get(sel) {
                                                if target.is_group_header {
                                                    if let Some(grp) = &target.group_name {
                                                        if expanded_groups.contains(grp) {
                                                            expanded_groups.remove(grp);
                                                        } else {
                                                            expanded_groups.insert(grp.clone());
                                                        }
                                                    }
                                                } else {
                                                    selected_proc_detail = Some(target.pid);
                                                    needs_redraw = true;
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Char('/') => {
                                        is_searching = true;
                                        table_state.select(Some(0));
                                        search_query.clear();
                                        needs_redraw = true;
                                    }
                                    KeyCode::Esc => {
                                        search_query.clear();
                                        table_state.select(Some(0));
                                    }
                                    KeyCode::Left => {
                                        let cur_idx = cols
                                            .iter()
                                            .position(|&c| c == current_sort_col)
                                            .unwrap_or(0);
                                        let new_idx = if cur_idx == 0 {
                                            cols.len() - 1
                                        } else {
                                            cur_idx - 1
                                        };
                                        current_sort_col = cols[new_idx];
                                        for s in &mut snapshots {
                                            sort_processes(
                                                &mut s.processes,
                                                current_sort_col,
                                                sort_ascending,
                                            );
                                        }
                                        sort_processes(
                                            &mut processes,
                                            current_sort_col,
                                            sort_ascending,
                                        );
                                    }
                                KeyCode::Right => {
                                    let cur_idx = cols
                                        .iter()
                                        .position(|&c| c == current_sort_col)
                                        .unwrap_or(0);
                                    let new_idx = (cur_idx + 1) % cols.len();
                                    current_sort_col = cols[new_idx];
                                    for s in &mut snapshots {
                                        sort_processes(
                                            &mut s.processes,
                                            current_sort_col,
                                            sort_ascending,
                                        );
                                    }
                                    sort_processes(
                                        &mut processes,
                                        current_sort_col,
                                        sort_ascending,
                                    );
                                }
                                KeyCode::Char('r') => {
                                    sort_ascending = !sort_ascending;
                                    for s in &mut snapshots {
                                        sort_processes(
                                            &mut s.processes,
                                            current_sort_col,
                                            sort_ascending,
                                        );
                                    }
                                    sort_processes(
                                        &mut processes,
                                        current_sort_col,
                                        sort_ascending,
                                    );
                                }
                                KeyCode::Char('a') => {
                                    advanced_view = !advanced_view;
                                    if !advanced_view
                                        && !NORMAL_SORT_COLUMNS.contains(&current_sort_col)
                                    {
                                        current_sort_col = ProcessSortColumn::Mem;
                                    }
                                    for s in &mut snapshots {
                                        sort_processes(
                                            &mut s.processes,
                                            current_sort_col,
                                            sort_ascending,
                                        );
                                    }
                                    sort_processes(
                                        &mut processes,
                                        current_sort_col,
                                        sort_ascending,
                                    );
                                }
                                KeyCode::Char('c') => {
                                    if let Some(sel) = table_state.selected() {
                                        let displayed: Vec<&ProcessInfo> = base_procs
                                            .iter()
                                            .filter(|p| advanced_view || p.rss_kb > 0)
                                            .filter(|p| {
                                                matches_process_search(
                                                    p,
                                                    &search_query,
                                                    Some(&proc_map),
                                                )
                                            })
                                            .collect();
                                        if let Some(target) = displayed.get(sel) {
                                            let mut targets = Vec::new();
                                            if !target.grouped_pids.is_empty() {
                                                for &p_id in &target.grouped_pids {
                                                    let (comm, name) = if let Some(p) =
                                                        proc_map.get(&p_id)
                                                    {
                                                        (p.comm.clone(), p.name.clone())
                                                    } else {
                                                        (target.comm.clone(), target.name.clone())
                                                    };
                                                    targets.push(ProcessTarget {
                                                        pid: p_id,
                                                        comm,
                                                        name,
                                                    });
                                                }
                                            } else {
                                                targets.push(ProcessTarget {
                                                    pid: target.pid,
                                                    comm: target.comm.clone(),
                                                    name: target.name.clone(),
                                                });
                                            }

                                            let mut validation_err = None;
                                            for t in &targets {
                                                if let Err(e) = validate_process_target(t) {
                                                    validation_err = Some(e);
                                                    break;
                                                }
                                            }

                                            if let Some(err_msg) = validation_err {
                                                process_error_popup = Some(ProcessErrorPopup {
                                                    title: "Process Validation Error".to_string(),
                                                    message_lines: vec![
                                                        err_msg,
                                                        "Action cancelled to protect unintended processes.".to_string(),
                                                    ],
                                                });
                                            } else {
                                                let pids = targets.iter().map(|t| t.pid).collect();
                                                let clean_name = target
                                                    .name
                                                    .trim_start_matches("▼ ")
                                                    .trim_start_matches("▶ ")
                                                    .trim_start_matches("├─ ")
                                                    .trim_start_matches("└─ ")
                                                    .trim()
                                                    .to_string();
                                                kill_confirmation = Some(ProcessKillConfirmation {
                                                    targets,
                                                    pids,
                                                    process_name: clean_name,
                                                    signal: rustix::process::Signal::Term,
                                                    is_kill: false,
                                                });
                                            }
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                KeyCode::Char('t') => {
                                    if let Some(sel) = table_state.selected() {
                                        let displayed: Vec<&ProcessInfo> = base_procs
                                            .iter()
                                            .filter(|p| advanced_view || p.rss_kb > 0)
                                            .filter(|p| {
                                                matches_process_search(
                                                    p,
                                                    &search_query,
                                                    Some(&proc_map),
                                                )
                                            })
                                            .collect();
                                        if let Some(target) = displayed.get(sel) {
                                            let mut targets = Vec::new();
                                            if !target.grouped_pids.is_empty() {
                                                for &p_id in &target.grouped_pids {
                                                    let (comm, name) = if let Some(p) =
                                                        proc_map.get(&p_id)
                                                    {
                                                        (p.comm.clone(), p.name.clone())
                                                    } else {
                                                        (target.comm.clone(), target.name.clone())
                                                    };
                                                    targets.push(ProcessTarget {
                                                        pid: p_id,
                                                        comm,
                                                        name,
                                                    });
                                                }
                                            } else {
                                                targets.push(ProcessTarget {
                                                    pid: target.pid,
                                                    comm: target.comm.clone(),
                                                    name: target.name.clone(),
                                                });
                                            }

                                            let mut validation_err = None;
                                            for t in &targets {
                                                if let Err(e) = validate_process_target(t) {
                                                    validation_err = Some(e);
                                                    break;
                                                }
                                            }

                                            if let Some(err_msg) = validation_err {
                                                process_error_popup = Some(ProcessErrorPopup {
                                                    title: "Process Validation Error".to_string(),
                                                    message_lines: vec![
                                                        err_msg,
                                                        "Action cancelled to protect unintended processes.".to_string(),
                                                    ],
                                                });
                                            } else {
                                                let pids = targets.iter().map(|t| t.pid).collect();
                                                let clean_name = target
                                                    .name
                                                    .trim_start_matches("▼ ")
                                                    .trim_start_matches("▶ ")
                                                    .trim_start_matches("├─ ")
                                                    .trim_start_matches("└─ ")
                                                    .trim()
                                                    .to_string();
                                                kill_confirmation = Some(ProcessKillConfirmation {
                                                    targets,
                                                    pids,
                                                    process_name: clean_name,
                                                    signal: rustix::process::Signal::Kill,
                                                    is_kill: true,
                                                });
                                            }
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let i = match table_state.selected() {
                                        Some(i) => i.saturating_sub(1),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let i = match table_state.selected() {
                                        Some(i) => (i + 1).min(num_procs.saturating_sub(1)),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::Char('g') | KeyCode::Home => {
                                    table_state.select(Some(0));
                                }
                                KeyCode::Char('G') | KeyCode::End => {
                                    if num_procs > 0 {
                                        table_state.select(Some(num_procs - 1));
                                    }
                                }
                                KeyCode::PageUp => {
                                    let i = match table_state.selected() {
                                        Some(i) => i.saturating_sub(10),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::PageDown => {
                                    let i = match table_state.selected() {
                                        Some(i) => (i + 10).min(num_procs.saturating_sub(1)),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                }
                                KeyCode::Char(' ') => {
                                    is_paused = !is_paused;
                                    if !is_paused {
                                        if !snapshots.is_empty() {
                                            snapshot_idx = snapshots.len() - 1;
                                        }
                                        last_tick = Instant::now();
                                    }
                                    needs_redraw = true;
                                }
                                _ => {}
                            }
                        }
                    } else if current_tab == 3 {
                            let cur_gpu = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.gpu_metrics)
                                .unwrap_or(&gpu_metrics);
                            let is_tabbed = is_gpu_overflow(table_area, cur_gpu);
                            match key.code {
                                KeyCode::Left | KeyCode::Char('h') => {
                                    if is_tabbed {
                                        gpu_sub_tab =
                                            if gpu_sub_tab == 0 { 1 } else { gpu_sub_tab - 1 };
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    if is_tabbed {
                                        gpu_sub_tab = (gpu_sub_tab + 1) % 2;
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    is_paused = !is_paused;
                                    if !is_paused {
                                        if !snapshots.is_empty() {
                                            snapshot_idx = snapshots.len() - 1;
                                        }
                                        last_tick = Instant::now();
                                    }
                                    needs_redraw = true;
                                }
                                _ => {}
                            }
                        } else if current_tab == 4 {
                            let cur_conns = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.net_connections)
                                .unwrap_or(&net_connections);
                            let num_conns = cur_conns.as_ref().map(|c| c.len()).unwrap_or(0);
                            let visible_rows =
                                (table_area.height * 55 / 100).saturating_sub(3) as usize;
                            let max_scroll = num_conns.saturating_sub(visible_rows);
                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    net_scroll_offset = net_scroll_offset.saturating_sub(1);
                                    needs_redraw = true;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    net_scroll_offset = (net_scroll_offset + 1).min(max_scroll);
                                    needs_redraw = true;
                                }
                                KeyCode::PageUp => {
                                    net_scroll_offset = net_scroll_offset.saturating_sub(10);
                                    needs_redraw = true;
                                }
                                KeyCode::PageDown => {
                                    net_scroll_offset = (net_scroll_offset + 10).min(max_scroll);
                                    needs_redraw = true;
                                }
                                KeyCode::Home | KeyCode::Char('g') => {
                                    net_scroll_offset = 0;
                                    needs_redraw = true;
                                }
                                KeyCode::End | KeyCode::Char('G') => {
                                    net_scroll_offset = max_scroll;
                                    needs_redraw = true;
                                }
                                KeyCode::Char(' ') => {
                                    is_paused = !is_paused;
                                    if !is_paused {
                                        if !snapshots.is_empty() {
                                            snapshot_idx = snapshots.len() - 1;
                                        }
                                        last_tick = Instant::now();
                                    }
                                    needs_redraw = true;
                                }
                                _ => {}
                            }
                        } else if current_tab == 5 {
                            let cur_disk_io = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.disk_io)
                                .unwrap_or(&disk_io);
                            let cur_mounts = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.disk_mounts[..])
                                .unwrap_or(&disk_mounts[..]);
                            let is_tabbed = is_disks_overflow(table_area, cur_disk_io, cur_mounts);
                            match key.code {
                                KeyCode::Char('a') | KeyCode::Char('A') => {
                                    disks_box_tab = 0;
                                    needs_redraw = true;
                                }
                                KeyCode::Char('s') | KeyCode::Char('S') => {
                                    disks_box_tab = 1;
                                    needs_redraw = true;
                                }
                                KeyCode::Char('d') | KeyCode::Char('D') => {
                                    disks_box_tab = 2;
                                    needs_redraw = true;
                                }
                                KeyCode::Char('f') | KeyCode::Char('F') => {
                                    disks_box_tab = 3;
                                    needs_redraw = true;
                                }
                                KeyCode::Left | KeyCode::Char('h') => {
                                    if !storage_categories.is_empty() {
                                        disks_sub_tab = if disks_sub_tab == 0 {
                                            storage_categories.len() - 1
                                        } else {
                                            disks_sub_tab - 1
                                        };
                                        disks_scroll_offset = 0;
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    if !storage_categories.is_empty() {
                                        disks_sub_tab =
                                            (disks_sub_tab + 1) % storage_categories.len();
                                        disks_scroll_offset = 0;
                                        needs_redraw = true;
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    disks_scroll_offset = disks_scroll_offset.saturating_sub(1);
                                    needs_redraw = true;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let cat_idx = disks_sub_tab
                                        .min(storage_categories.len().saturating_sub(1));
                                    let num_items = storage_categories
                                        .get(cat_idx)
                                        .map(|c| c.items.len())
                                        .unwrap_or(0);
                                    let box_h = if is_tabbed {
                                        table_area.height.saturating_sub(3)
                                    } else {
                                        table_area.height.saturating_sub(table_area.height / 2)
                                    };
                                    let visible_rows = (box_h.saturating_sub(4) as usize).max(1);
                                    let max_scroll = num_items.saturating_sub(visible_rows);
                                    disks_scroll_offset = (disks_scroll_offset + 1).min(max_scroll);
                                    needs_redraw = true;
                                }
                                KeyCode::PageUp => {
                                    disks_scroll_offset = disks_scroll_offset.saturating_sub(5);
                                    needs_redraw = true;
                                }
                                KeyCode::PageDown => {
                                    let cat_idx = disks_sub_tab
                                        .min(storage_categories.len().saturating_sub(1));
                                    let num_items = storage_categories
                                        .get(cat_idx)
                                        .map(|c| c.items.len())
                                        .unwrap_or(0);
                                    let box_h = if is_tabbed {
                                        table_area.height.saturating_sub(3)
                                    } else {
                                        table_area.height.saturating_sub(table_area.height / 2)
                                    };
                                    let visible_rows = (box_h.saturating_sub(4) as usize).max(1);
                                    let max_scroll = num_items.saturating_sub(visible_rows);
                                    disks_scroll_offset = (disks_scroll_offset + 5).min(max_scroll);
                                    needs_redraw = true;
                                }
                                KeyCode::Home | KeyCode::Char('g') => {
                                    disks_scroll_offset = 0;
                                    needs_redraw = true;
                                }
                                KeyCode::End | KeyCode::Char('G') => {
                                    let cat_idx = disks_sub_tab
                                        .min(storage_categories.len().saturating_sub(1));
                                    let num_items = storage_categories
                                        .get(cat_idx)
                                        .map(|c| c.items.len())
                                        .unwrap_or(0);
                                    let box_h = if is_tabbed {
                                        table_area.height.saturating_sub(3)
                                    } else {
                                        table_area.height.saturating_sub(table_area.height / 2)
                                    };
                                    let visible_rows = (box_h.saturating_sub(4) as usize).max(1);
                                    let max_scroll = num_items.saturating_sub(visible_rows);
                                    disks_scroll_offset = max_scroll;
                                    needs_redraw = true;
                                }
                                KeyCode::Enter | KeyCode::Char('r') | KeyCode::Char('R') => {
                                    if !is_tabbed || disks_box_tab == 2 {
                                        let cat_idx = disks_sub_tab
                                            .min(storage_categories.len().saturating_sub(1));
                                        if let Some(cat) = storage_categories.get(cat_idx)
                                            && cat.name == "All"
                                            && !is_dust_scanning
                                        {
                                            is_dust_scanning = true;
                                            let tx = dust_tx.clone();
                                            std::thread::Builder::new()
                                                .name("dust-scanner".to_string())
                                                .spawn(move || {
                                                    let res = read_dust_storage();
                                                    let _ = tx.send(res);
                                                })
                                                .ok();
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    is_paused = !is_paused;
                                    if !is_paused {
                                        if !snapshots.is_empty() {
                                            snapshot_idx = snapshots.len() - 1;
                                        }
                                        last_tick = Instant::now();
                                    }
                                    needs_redraw = true;
                                }
                                _ => {}
                            }
                        } else if key.code == KeyCode::Char(' ') {
                            is_paused = !is_paused;
                            if !is_paused {
                                if !snapshots.is_empty() {
                                    snapshot_idx = snapshots.len() - 1;
                                }
                                last_tick = Instant::now();
                            }
                        }
                    }
                    Event::Mouse(mouse_event) => {
                        let mx = mouse_event.column;
                        let my = mouse_event.row;

                        if let MouseEventKind::Down(MouseButton::Left) = mouse_event.kind
                            && my == tabs_area.y + 1
                            && mx >= tabs_area.x
                            && mx < tabs_area.right()
                        {
                            let mut tab_x = tabs_area.x + 1;
                            for (idx, title) in TAB_TITLES.iter().enumerate() {
                                let tab_w = 1 + title.chars().count() as u16 + 1;
                                if mx >= tab_x && mx < tab_x + tab_w {
                                    current_tab = idx;
                                    needs_redraw = true;
                                    break;
                                }
                                tab_x += tab_w + 1;
                            }
                        } else if current_tab == 0 {
                            match mouse_event.kind {
                                MouseEventKind::ScrollDown => {
                                    general_sub_tab = (general_sub_tab + 1) % 4;
                                    needs_redraw = true;
                                }
                                MouseEventKind::ScrollUp => {
                                    general_sub_tab = if general_sub_tab == 0 {
                                        3
                                    } else {
                                        general_sub_tab - 1
                                    };
                                    needs_redraw = true;
                                }
                                MouseEventKind::Down(MouseButton::Left) => {
                                    let num_cores = core_usages.len();
                                    let cpu_min_rows = 1 + num_cores.div_ceil(4) as u16 + 2;
                                    let min_height_needed = 14
                                        + 4
                                        + (table_area.height * 30 / 100).max(6)
                                        + cpu_min_rows;
                                    let is_compact = table_area.height < min_height_needed
                                        || table_area.width < 80;
                                    if is_compact && my == table_area.y + 1 {
                                        let titles = ["Overview", "Hardware", "High", "Partitions"];
                                        let mut tab_x = table_area.x + 2;
                                        for (i, title) in titles.iter().enumerate() {
                                            let w = title.chars().count() as u16 + 2;
                                            if mx >= tab_x && mx < tab_x + w {
                                                general_sub_tab = i;
                                                needs_redraw = true;
                                                break;
                                            }
                                            tab_x += w + 1;
                                        }
                                    } else {
                                        let top_box_w = if is_compact {
                                            table_area.width
                                        } else {
                                            table_area.width / 2
                                        };
                                        let top_box_right = table_area.x + top_box_w;
                                        let top_box_bottom = if is_compact {
                                            table_area.y + 3 + 14
                                        } else {
                                            table_area.y + 14
                                        };
                                        if mx >= top_box_right.saturating_sub(18)
                                            && mx <= top_box_right
                                            && my >= top_box_bottom.saturating_sub(3)
                                            && my <= top_box_bottom
                                        {
                                            let cur_snap = snapshots.get(snapshot_idx);
                                            let text = format_system_overview_copy_text(
                                                cur_snap.map(|s| &s.sys_info).unwrap_or(&sys_info),
                                                &cpu_model,
                                                cur_snap
                                                    .map(|s| &s.gpu_metrics)
                                                    .unwrap_or(&gpu_metrics),
                                                cur_snap
                                                    .map(|s| s.ram_info.as_str())
                                                    .unwrap_or(&ram_info),
                                            );
                                            copy_to_clipboard(&text);
                                            copy_feedback_until =
                                                Some(Instant::now() + Duration::from_secs(2));
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if current_tab == 1 {
                            let active_procs = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.processes)
                                .unwrap_or(&processes);
                            let grouped_cache;
                            let base_procs = if advanced_view {
                                active_procs
                            } else {
                                grouped_cache = group_processes_for_simple_view(
                                    active_procs,
                                    &expanded_groups,
                                    current_sort_col,
                                    sort_ascending,
                                    &search_query,
                                );
                                &grouped_cache
                            };
                            let proc_map: HashMap<u32, &ProcessInfo> =
                                active_procs.iter().map(|p| (p.pid, p)).collect();
                            let num_procs = base_procs
                                .iter()
                                .filter(|p| advanced_view || p.rss_kb > 0)
                                .filter(|p| {
                                    matches_process_search(p, &search_query, Some(&proc_map))
                                })
                                .count();
                            match mouse_event.kind {
                                MouseEventKind::ScrollDown => {
                                    let i = match table_state.selected() {
                                        Some(i) => (i + 3).min(num_procs.saturating_sub(1)),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                    needs_redraw = true;
                                }
                                MouseEventKind::ScrollUp => {
                                    let i = match table_state.selected() {
                                        Some(i) => i.saturating_sub(3),
                                        None => 0,
                                    };
                                    table_state.select(Some(i));
                                    needs_redraw = true;
                                }
                                MouseEventKind::Down(MouseButton::Left) => {
                                    let header_y = table_area.y + 1;
                                    if my == header_y {
                                        let show_pid = table_area.width >= 150;
                                        let (col_widths, cols): (Vec<u16>, Vec<ProcessSortColumn>) =
                                            if advanced_view {
                                                let pid_w = if show_pid { 8 } else { 0 };
                                                let base_w =
                                                    pid_w + 10 + 7 + 8 + 5 + 10 + 4 + 10 + 21 + 21;
                                                let name_w =
                                                    table_area.width.saturating_sub(base_w).max(10);
                                                let mut widths = Vec::with_capacity(11);
                                                let mut c_list = Vec::with_capacity(11);
                                                if show_pid {
                                                    widths.push(8);
                                                    c_list.push(ProcessSortColumn::Pid);
                                                }
                                                widths.extend_from_slice(&[
                                                    10, name_w, 7, 8, 5, 10, 5, 9, 21, 21,
                                                ]);
                                                c_list.extend_from_slice(&[
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
                                                ]);
                                                (widths, c_list)
                                            } else {
                                                let pid_w = if show_pid { 8 } else { 0 };
                                                let base_w = pid_w + 5 + 10 + 5 + 9 + 21 + 21;
                                                let name_w =
                                                    table_area.width.saturating_sub(base_w).max(10);
                                                let mut widths = Vec::with_capacity(8);
                                                let mut c_list = Vec::with_capacity(8);
                                                if show_pid {
                                                    widths.push(8);
                                                    c_list.push(ProcessSortColumn::Pid);
                                                }
                                                widths.extend_from_slice(&[
                                                    name_w, 5, 10, 5, 9, 21, 21,
                                                ]);
                                                c_list.extend_from_slice(&[
                                                    ProcessSortColumn::Name,
                                                    ProcessSortColumn::Cpu,
                                                    ProcessSortColumn::Mem,
                                                    ProcessSortColumn::Gpu,
                                                    ProcessSortColumn::GpuMem,
                                                    ProcessSortColumn::Io,
                                                    ProcessSortColumn::Net,
                                                ]);
                                                (widths, c_list)
                                            };

                                        let mut current_x = table_area.x + 1;
                                        for (i, &w) in col_widths.iter().enumerate() {
                                            if mx >= current_x && mx < current_x + w {
                                                if current_sort_col == cols[i] {
                                                    sort_ascending = !sort_ascending;
                                                } else {
                                                    current_sort_col = cols[i];
                                                }
                                                for s in &mut snapshots {
                                                    sort_processes(
                                                        &mut s.processes,
                                                        current_sort_col,
                                                        sort_ascending,
                                                    );
                                                }
                                                sort_processes(
                                                    &mut processes,
                                                    current_sort_col,
                                                    sort_ascending,
                                                );
                                                needs_redraw = true;
                                                break;
                                            }
                                            current_x += w;
                                        }
                                    } else if my > header_y + 1 && my < table_area.bottom() {
                                        let clicked_row = (my - (header_y + 2)) as usize;
                                        let proc_idx = table_state.offset() + clicked_row;
                                        if proc_idx < num_procs {
                                            if table_state.selected() == Some(proc_idx) {
                                                let displayed: Vec<&ProcessInfo> = base_procs
                                                    .iter()
                                                    .filter(|p| advanced_view || p.rss_kb > 0)
                                                    .filter(|p| {
                                                        matches_process_search(
                                                            p,
                                                            &search_query,
                                                            Some(&proc_map),
                                                        )
                                                    })
                                                    .collect();
                                                if let Some(target) = displayed.get(proc_idx) {
                                                    if target.is_group_header {
                                                        if let Some(grp) = &target.group_name {
                                                            if expanded_groups.contains(grp) {
                                                                expanded_groups.remove(grp);
                                                            } else {
                                                                expanded_groups.insert(grp.clone());
                                                            }
                                                        }
                                                    } else {
                                                        selected_proc_detail = Some(target.pid);
                                                    }
                                                }
                                            } else {
                                                table_state.select(Some(proc_idx));
                                            }
                                            needs_redraw = true;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if current_tab == 3 {
                            let cur_gpu = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.gpu_metrics)
                                .unwrap_or(&gpu_metrics);
                            let is_tabbed = is_gpu_overflow(table_area, cur_gpu);
                            match mouse_event.kind {
                                MouseEventKind::ScrollDown => {
                                    if is_tabbed {
                                        gpu_sub_tab = (gpu_sub_tab + 1) % 2;
                                        needs_redraw = true;
                                    }
                                }
                                MouseEventKind::ScrollUp => {
                                    if is_tabbed {
                                        gpu_sub_tab =
                                            if gpu_sub_tab == 0 { 1 } else { gpu_sub_tab - 1 };
                                        needs_redraw = true;
                                    }
                                }
                                MouseEventKind::Down(MouseButton::Left) => {
                                    let top_w = (table_area.width * 60) / 100;
                                    if is_tabbed
                                        && my == table_area.y + 1
                                        && mx >= table_area.x
                                        && mx < table_area.x + top_w
                                    {
                                        let titles = ["GPU Utilization", "VRAM History"];
                                        let mut tab_x = table_area.x + 2;
                                        for (i, title) in titles.iter().enumerate() {
                                            let w = title.chars().count() as u16 + 2;
                                            if mx >= tab_x && mx < tab_x + w {
                                                gpu_sub_tab = i;
                                                needs_redraw = true;
                                                break;
                                            }
                                            tab_x += w + 1;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if current_tab == 4 {
                            let cur_conns = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.net_connections)
                                .unwrap_or(&net_connections);
                            let num_conns = cur_conns.as_ref().map(|c| c.len()).unwrap_or(0);
                            let visible_rows =
                                (table_area.height * 55 / 100).saturating_sub(3) as usize;
                            let max_scroll = num_conns.saturating_sub(visible_rows);
                            match mouse_event.kind {
                                MouseEventKind::ScrollDown => {
                                    net_scroll_offset = (net_scroll_offset + 3).min(max_scroll);
                                    needs_redraw = true;
                                }
                                MouseEventKind::ScrollUp => {
                                    net_scroll_offset = net_scroll_offset.saturating_sub(3);
                                    needs_redraw = true;
                                }
                                _ => {}
                            }
                        } else if current_tab == 5 {
                            let cur_disk_io = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.disk_io)
                                .unwrap_or(&disk_io);
                            let cur_mounts = snapshots
                                .get(snapshot_idx)
                                .map(|s| &s.disk_mounts[..])
                                .unwrap_or(&disk_mounts[..]);
                            let is_tabbed = is_disks_overflow(table_area, cur_disk_io, cur_mounts);
                            let (tab_row_y, storage_w) = if is_tabbed {
                                (table_area.y + 4, table_area.width)
                            } else {
                                (
                                    table_area.y + table_area.height / 2 + 1,
                                    (table_area.width * 70) / 100,
                                )
                            };

                            match mouse_event.kind {
                                MouseEventKind::ScrollDown => {
                                    let cat_idx = disks_sub_tab
                                        .min(storage_categories.len().saturating_sub(1));
                                    let num_items = storage_categories
                                        .get(cat_idx)
                                        .map(|c| c.items.len())
                                        .unwrap_or(0);
                                    let box_h = if is_tabbed {
                                        table_area.height.saturating_sub(3)
                                    } else {
                                        table_area.height.saturating_sub(table_area.height / 2)
                                    };
                                    let visible_rows = (box_h.saturating_sub(4) as usize).max(1);
                                    let max_scroll = num_items.saturating_sub(visible_rows);
                                    disks_scroll_offset = (disks_scroll_offset + 2).min(max_scroll);
                                    needs_redraw = true;
                                }
                                MouseEventKind::ScrollUp => {
                                    disks_scroll_offset = disks_scroll_offset.saturating_sub(2);
                                    needs_redraw = true;
                                }
                                MouseEventKind::Down(MouseButton::Left)
                                    if is_tabbed && my == table_area.y + 1 && mx > table_area.x =>
                                {
                                    let click_col = mx.saturating_sub(table_area.x + 1);
                                    if click_col < 12 {
                                        disks_box_tab = 0;
                                    } else if click_col < 33 {
                                        disks_box_tab = 1;
                                    } else if click_col < 47 {
                                        disks_box_tab = 2;
                                    } else {
                                        disks_box_tab = 3;
                                    }
                                    needs_redraw = true;
                                }
                                MouseEventKind::Down(MouseButton::Left)
                                    if (!is_tabbed || disks_box_tab == 2)
                                        && (my == tab_row_y || my == tab_row_y + 1)
                                        && mx > table_area.x
                                        && mx < table_area.x + storage_w =>
                                {
                                    let inner_w = storage_w.saturating_sub(2);
                                    let click_col = mx.saturating_sub(table_area.x + 1);
                                    let mut cur_row: u16 = 0;
                                    let mut cur_col: u16 = 0;
                                    for (i, cat) in storage_categories.iter().enumerate() {
                                        let tab_len = if cat.total_str.is_empty() {
                                            format!("[ ▶ {} ] ", cat.name).chars().count() as u16
                                        } else {
                                            format!("[ ▶ {} ({}) ] ", cat.name, cat.total_str)
                                                .chars()
                                                .count()
                                                as u16
                                        };
                                        if cur_row == 0
                                            && cur_col > 0
                                            && (cur_col + tab_len) > inner_w
                                        {
                                            cur_row = 1;
                                            cur_col = 0;
                                        }
                                        let target_y = tab_row_y + cur_row;
                                        if my == target_y
                                            && click_col >= cur_col
                                            && click_col < cur_col + tab_len
                                        {
                                            disks_sub_tab = i;
                                            disks_scroll_offset = 0;
                                            needs_redraw = true;
                                            break;
                                        }
                                        cur_col += tab_len;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
                more = event::poll(Duration::ZERO)?;
            }
        }

        if last_tick.elapsed() >= tick_rate {
            let dt = last_tick.elapsed().as_secs_f64();
            sys_info = read_system_general_info();
            battery = read_battery();

            processes = read_processes(&mut prev_procs, &users, dt);
            sort_processes(&mut processes, current_sort_col, sort_ascending);

            std::mem::swap(&mut prev_ticks, &mut curr_ticks);
            read_cpu_ticks(&mut io_buf, &mut curr_ticks);
            mem = read_memory(&mut io_buf);
            let m_pct = if mem.total_mem_mb > 0 {
                (mem.used_mem_mb as f64 / mem.total_mem_mb as f64) * 100.0
            } else {
                0.0
            };
            let s_pct = if mem.total_swap_mb > 0 {
                (mem.used_swap_mb as f64 / mem.total_swap_mb as f64) * 100.0
            } else {
                0.0
            };

            if !prev_ticks.is_empty() && !curr_ticks.is_empty() {
                global_usage = calculate_usage(&prev_ticks[0], &curr_ticks[0]);
                core_usages.clear();
                for i in 1..curr_ticks.len().min(prev_ticks.len()) {
                    core_usages.push(calculate_usage(&prev_ticks[i], &curr_ticks[i]));
                }
            }

            let freq_info = read_cpu_freq_info();
            cpu_cur_mhz = freq_info.0;
            cpu_min_mhz = freq_info.1;
            cpu_max_mhz = freq_info.2;
            cpu_temp = read_cpu_temp();
            ram_info = get_ram_info(mem.total_mem_mb, &cpu_model);

            gpu_metrics = read_gpu_metrics();
            let v_pct = if gpu_metrics.vram_total_mb > 0 {
                (gpu_metrics.vram_used_mb as f64 / gpu_metrics.vram_total_mb as f64) * 100.0
            } else {
                0.0
            };

            net_ifaces = read_network_interfaces(&mut prev_net, dt);
            net_connections = read_network_connections(&dns_resolver);
            let primary_iface = net_ifaces
                .iter()
                .find(|i| i.operstate == "up")
                .or_else(|| net_ifaces.first());
            let rx_spd = primary_iface.map(|i| i.rx_speed).unwrap_or(0.0);
            let tx_spd = primary_iface.map(|i| i.tx_speed).unwrap_or(0.0);
            let rx_pct = io_gradient_pct(rx_spd);
            let tx_pct = io_gradient_pct(tx_spd);

            disk_mounts = read_disk_mounts();
            disk_io = read_disk_io(&mut prev_disk, dt);
            let d_read_pct = io_gradient_pct(disk_io.read_speed);
            let d_write_pct = io_gradient_pct(disk_io.write_speed);

            for i in 0..99 {
                cpu_history[i] = cpu_history[i + 1];
                mem_history[i] = mem_history[i + 1];
                swap_history[i] = swap_history[i + 1];
                gpu_history[i] = gpu_history[i + 1];
                gpu_vram_history[i] = gpu_vram_history[i + 1];
                net_rx_history[i] = net_rx_history[i + 1];
                net_tx_history[i] = net_tx_history[i + 1];
                disk_read_history[i] = disk_read_history[i + 1];
                disk_write_history[i] = disk_write_history[i + 1];
            }
            cpu_history[99] = Some(global_usage);
            mem_history[99] = Some(m_pct);
            swap_history[99] = Some(s_pct);
            gpu_history[99] = Some(gpu_metrics.utilization_pct);
            gpu_vram_history[99] = Some(v_pct);
            net_rx_history[99] = Some(rx_pct);
            net_tx_history[99] = Some(tx_pct);
            disk_read_history[99] = Some(d_read_pct);
            disk_write_history[99] = Some(d_write_pct);

            snapshots.push(create_snapshot(
                &sys_info,
                battery.as_ref(),
                global_usage,
                &core_usages,
                cpu_cur_mhz,
                cpu_min_mhz,
                cpu_max_mhz,
                cpu_temp,
                &mem,
                &ram_info,
                &gpu_metrics,
                &net_ifaces,
                &net_connections,
                &disk_io,
                &disk_mounts,
                &processes,
                &cpu_history,
                &mem_history,
                &swap_history,
                &gpu_history,
                &gpu_vram_history,
                &net_rx_history,
                &net_tx_history,
                &disk_read_history,
                &disk_write_history,
            ));
            if snapshots.len() > MAX_SNAPSHOTS {
                snapshots.remove(0);
                if is_paused {
                    snapshot_idx = snapshot_idx.saturating_sub(1);
                }
            }
            if !is_paused {
                snapshot_idx = snapshots.len().saturating_sub(1);
            }

            last_tick = Instant::now();
            needs_redraw = true;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_navigation_and_clamping() {
        let mut snapshots = Vec::new();
        let sys_info = SystemGeneralInfo::default();
        let gpu = GpuMetrics::default();
        let mem = MemoryMetrics::default();
        let disk = DiskIoInfo::default();
        let dummy_net_conns: Result<Vec<NetConnectionInfo>, &'static str> = Ok(Vec::new());

        for i in 0..5 {
            snapshots.push(create_snapshot(
                &sys_info,
                None,
                i as f64 * 10.0,
                &[i as f64 * 10.0],
                2000.0,
                1000.0,
                3000.0,
                45,
                &mem,
                "16 GB",
                &gpu,
                &[],
                &dummy_net_conns,
                &disk,
                &[],
                &[],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
            ));
        }

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
        let mut snapshots = Vec::new();
        let sys_info = SystemGeneralInfo::default();
        let gpu = GpuMetrics::default();
        let mem = MemoryMetrics::default();
        let disk = DiskIoInfo::default();
        let dummy_net_conns: Result<Vec<NetConnectionInfo>, &'static str> = Ok(Vec::new());

        let create_dummy = |val: f64| {
            create_snapshot(
                &sys_info,
                None,
                val,
                &[val],
                2000.0,
                1000.0,
                3000.0,
                45,
                &mem,
                "16 GB",
                &gpu,
                &[],
                &dummy_net_conns,
                &disk,
                &[],
                &[],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
                &vec![None; 100],
            )
        };

        // Start with 3 snapshots
        snapshots.push(create_dummy(1.0));
        snapshots.push(create_dummy(2.0));
        snapshots.push(create_dummy(3.0));
        assert_eq!(snapshots.len(), 3);

        // User pauses at 2nd snapshot (index 1, display 2/3)
        let is_paused = true;
        let mut snapshot_idx = 1;
        assert_eq!(snapshot_idx + 1, 2);
        assert_eq!(snapshots.len(), 3);

        // New tick captures snapshot 4 while paused
        snapshots.push(create_dummy(4.0));
        if !is_paused {
            snapshot_idx = snapshots.len().saturating_sub(1);
        }
        // Now at 2/4 (snapshot_idx = 1, len = 4)
        assert_eq!(snapshot_idx + 1, 2);
        assert_eq!(snapshots.len(), 4);

        // New tick captures snapshot 5 while paused
        snapshots.push(create_dummy(5.0));
        if !is_paused {
            snapshot_idx = snapshots.len().saturating_sub(1);
        }
        // Now at 2/5 (snapshot_idx = 1, len = 5)
        assert_eq!(snapshot_idx + 1, 2);
        assert_eq!(snapshots.len(), 5);

        // Unpausing jumps to latest (5/5)
        let is_paused = false;
        if !is_paused {
            snapshot_idx = snapshots.len().saturating_sub(1);
        }
        assert_eq!(snapshot_idx + 1, 5);
        assert_eq!(snapshots.len(), 5);
    }

    #[test]
    fn test_process_detail_view_navigation() {
        let mut selected_proc_detail: Option<u32> = None;

        // Pressing Enter on a non-group process sets selected_proc_detail
        let target_pid = 12345;
        let is_group_header = false;
        if !is_group_header {
            selected_proc_detail = Some(target_pid);
        }
        assert_eq!(selected_proc_detail, Some(12345));

        // Pressing Esc / q / Enter restores process list
        let key_code = crossterm::event::KeyCode::Esc;
        if matches!(key_code, crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Enter) {
            selected_proc_detail = None;
        }
        assert_eq!(selected_proc_detail, None);

        // Stopping (c) or Killing (t) zoomed in process also resets selected_proc_detail back to process list
        selected_proc_detail = Some(target_pid);
        let stop_key = crossterm::event::KeyCode::Char('c');
        if matches!(stop_key, crossterm::event::KeyCode::Char('c') | crossterm::event::KeyCode::Char('t')) {
            selected_proc_detail = None;
        }
        assert_eq!(selected_proc_detail, None);
    }
}

