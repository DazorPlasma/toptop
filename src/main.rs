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
    collections::HashMap,
    io,
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
    style::{Color, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, TableState, Tabs},
};

use crate::{
    process::{
        ADVANCED_SORT_COLUMNS, NORMAL_SORT_COLUMNS, ProcessInfo, ProcessSortColumn, read_processes,
        sort_processes,
    },
    system::{
        calculate_usage, get_cpu_model, get_ram_info, get_users, read_battery, read_cpu_freq_info,
        read_cpu_temp, read_cpu_ticks, read_disk_io, read_disk_mounts, read_docker_storage,
        read_gpu_metrics, read_memory, read_network_interfaces, read_system_general_info,
    },
    theme::io_gradient_pct,
    ui::{
        format_system_overview_copy_text, render_cpu_ram_tab, render_disks_tab, render_general_tab,
        render_gpu_tab, render_network_tab, render_process_tab,
    },
    utils::{copy_to_clipboard, fuzzy_match},
};

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
    let mut docker_info = read_docker_storage();

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

    let mut table_area = Rect::default();
    let mut copy_feedback_until: Option<Instant> = None;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(area);

            table_area = chunks[1];

            let titles = vec![
                Line::from(" General (1) "),
                Line::from(" Processes (2) "),
                Line::from(" CPU & RAM (3) "),
                Line::from(" GPU (4) "),
                Line::from(" Network (5) "),
                Line::from(" Disks (6) "),
            ];
            let pause_badge = if is_paused { " [PAUSED] " } else { "" };
            let tabs_title = format!(
                " Rust System Monitor (toptop) - 'q' to quit, Space to pause{} ",
                pause_badge
            );
            let tabs = Tabs::new(titles)
                .style(Style::default().not_bold().fg(Color::Rgb(170, 170, 170)))
                .select(current_tab)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Plain)
                        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                        .title(tabs_title),
                )
                .highlight_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold())
                .divider("|");

            frame.render_widget(tabs, chunks[0]);

            let (cpu_cur_mhz, cpu_min_mhz, cpu_max_mhz) = read_cpu_freq_info();
            let cpu_temp = read_cpu_temp();
            let ram_info = get_ram_info(mem.total_mem_mb, &cpu_model);
            let is_copied = copy_feedback_until
                .map(|t| Instant::now() < t)
                .unwrap_or(false);

            match current_tab {
                0 => {
                    render_general_tab(
                        frame,
                        chunks[1],
                        &sys_info,
                        battery.as_ref(),
                        global_usage,
                        &core_usages,
                        cpu_cur_mhz,
                        cpu_min_mhz,
                        cpu_max_mhz,
                        cpu_temp,
                        &cpu_model,
                        &mem,
                        &ram_info,
                        &gpu_metrics,
                        &net_ifaces,
                        &disk_io,
                        &disk_mounts,
                        &processes,
                        is_copied,
                    );
                }
                1 => {
                    let num_cores = core_usages.len().max(1);
                    render_process_tab(
                        frame,
                        chunks[1],
                        &processes,
                        advanced_view,
                        &search_query,
                        is_searching,
                        current_sort_col,
                        sort_ascending,
                        mem.total_mem_mb,
                        num_cores,
                        &mut table_state,
                    );
                }
                2 => {
                    render_cpu_ram_tab(
                        frame,
                        chunks[1],
                        &cpu_history,
                        &core_usages,
                        cpu_cur_mhz,
                        cpu_min_mhz,
                        cpu_max_mhz,
                        cpu_temp,
                        &cpu_model,
                        &mem,
                        &mem_history,
                        &ram_info,
                    );
                }
                3 => {
                    render_gpu_tab(
                        frame,
                        chunks[1],
                        &gpu_metrics,
                        &gpu_history,
                        &gpu_vram_history,
                    );
                }
                4 => {
                    render_network_tab(
                        frame,
                        chunks[1],
                        &net_ifaces,
                        &net_rx_history,
                        &net_tx_history,
                    );
                }
                5 => {
                    render_disks_tab(
                        frame,
                        chunks[1],
                        &disk_io,
                        &disk_mounts,
                        &disk_read_history,
                        &disk_write_history,
                        &docker_info,
                    );
                }
                _ => {}
            }
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if is_searching {
                        let num_procs = processes
                            .iter()
                            .filter(|p| advanced_view || p.rss_kb > 0)
                            .filter(|p| {
                                search_query.is_empty()
                                    || fuzzy_match(&search_query, &p.name)
                                    || fuzzy_match(&search_query, &p.pid.to_string())
                                    || fuzzy_match(&search_query, &p.user)
                            })
                            .count();
                        match key.code {
                            KeyCode::Esc => {
                                is_searching = false;
                                search_query.clear();
                            }
                            KeyCode::Enter => {
                                is_searching = false;
                            }
                            KeyCode::Backspace => {
                                search_query.pop();
                            }
                            KeyCode::Char(c) => {
                                search_query.push(c);
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
                    } else if key.code == KeyCode::Char('q')
                        || (key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c'))
                    {
                        break;
                    } else if key.code == KeyCode::Tab {
                        current_tab = (current_tab + 1) % 6;
                    } else if key.code == KeyCode::BackTab {
                        current_tab = (current_tab + 5) % 6;
                    } else if key.code == KeyCode::Char('1') {
                        current_tab = 0;
                    } else if key.code == KeyCode::Char('2') {
                        current_tab = 1;
                    } else if key.code == KeyCode::Char('3') {
                        current_tab = 2;
                    } else if key.code == KeyCode::Char('4') {
                        current_tab = 3;
                    } else if key.code == KeyCode::Char('5') {
                        current_tab = 4;
                    } else if key.code == KeyCode::Char('6') {
                        current_tab = 5;
                    } else if current_tab == 0 {
                        if key.code == KeyCode::Char('c') || key.code == KeyCode::Char('y') {
                            let ram_info = get_ram_info(mem.total_mem_mb, &cpu_model);
                            let text = format_system_overview_copy_text(
                                &sys_info,
                                &cpu_model,
                                &gpu_metrics,
                                &ram_info,
                            );
                            copy_to_clipboard(&text);
                            copy_feedback_until = Some(Instant::now() + Duration::from_secs(2));
                        } else if key.code == KeyCode::Char(' ') {
                            is_paused = !is_paused;
                        }
                    } else if current_tab == 1 {
                        let cols: &[ProcessSortColumn] = if advanced_view {
                            &ADVANCED_SORT_COLUMNS
                        } else {
                            &NORMAL_SORT_COLUMNS
                        };
                        let num_procs = processes
                            .iter()
                            .filter(|p| advanced_view || p.rss_kb > 0)
                            .filter(|p| {
                                search_query.is_empty()
                                    || fuzzy_match(&search_query, &p.name)
                                    || fuzzy_match(&search_query, &p.pid.to_string())
                                    || fuzzy_match(&search_query, &p.user)
                            })
                            .count();
                        match key.code {
                            KeyCode::Char('/') => {
                                is_searching = true;
                            }
                            KeyCode::Esc => {
                                search_query.clear();
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
                                sort_processes(&mut processes, current_sort_col, sort_ascending);
                            }
                            KeyCode::Right => {
                                let cur_idx = cols
                                    .iter()
                                    .position(|&c| c == current_sort_col)
                                    .unwrap_or(0);
                                let new_idx = (cur_idx + 1) % cols.len();
                                current_sort_col = cols[new_idx];
                                sort_processes(&mut processes, current_sort_col, sort_ascending);
                            }
                            KeyCode::Char('r') => {
                                sort_ascending = !sort_ascending;
                                sort_processes(&mut processes, current_sort_col, sort_ascending);
                            }
                            KeyCode::Char('a') => {
                                advanced_view = !advanced_view;
                                if !advanced_view
                                    && !NORMAL_SORT_COLUMNS.contains(&current_sort_col)
                                {
                                    current_sort_col = ProcessSortColumn::Mem;
                                }
                                sort_processes(&mut processes, current_sort_col, sort_ascending);
                            }
                            KeyCode::Char('c') => {
                                if let Some(sel) = table_state.selected() {
                                    let displayed: Vec<&ProcessInfo> = processes
                                        .iter()
                                        .filter(|p| advanced_view || p.rss_kb > 0)
                                        .filter(|p| {
                                            search_query.is_empty()
                                                || fuzzy_match(&search_query, &p.name)
                                                || fuzzy_match(&search_query, &p.pid.to_string())
                                                || fuzzy_match(&search_query, &p.user)
                                        })
                                        .collect();
                                    if let Some(target) = displayed.get(sel)
                                        && let Some(pid) =
                                            rustix::process::Pid::from_raw(target.pid as i32)
                                    {
                                        let _ = rustix::process::kill_process(
                                            pid,
                                            rustix::process::Signal::Term,
                                        );
                                    }
                                }
                            }
                            KeyCode::Char('k') => {
                                if let Some(sel) = table_state.selected() {
                                    let displayed: Vec<&ProcessInfo> = processes
                                        .iter()
                                        .filter(|p| advanced_view || p.rss_kb > 0)
                                        .filter(|p| {
                                            search_query.is_empty()
                                                || fuzzy_match(&search_query, &p.name)
                                                || fuzzy_match(&search_query, &p.pid.to_string())
                                                || fuzzy_match(&search_query, &p.user)
                                        })
                                        .collect();
                                    if let Some(target) = displayed.get(sel)
                                        && let Some(pid) =
                                            rustix::process::Pid::from_raw(target.pid as i32)
                                    {
                                        let _ = rustix::process::kill_process(
                                            pid,
                                            rustix::process::Signal::Kill,
                                        );
                                    }
                                }
                            }
                            KeyCode::Up => {
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
                            }
                            _ => {}
                        }
                    } else if key.code == KeyCode::Char(' ') {
                        is_paused = !is_paused;
                    }
                }
                Event::Mouse(mouse_event) => {
                    if current_tab == 0 {
                        if let MouseEventKind::Down(MouseButton::Left) = mouse_event.kind {
                            let mx = mouse_event.column;
                            let my = mouse_event.row;
                            let top_box_w = table_area.width / 2;
                            let top_box_right = table_area.x + top_box_w;
                            let top_box_bottom = table_area.y + 15;
                            if mx >= top_box_right.saturating_sub(18)
                                && mx <= top_box_right
                                && my >= top_box_bottom.saturating_sub(3)
                                && my <= top_box_bottom
                            {
                                let ram_info = get_ram_info(mem.total_mem_mb, &cpu_model);
                                let text = format_system_overview_copy_text(
                                    &sys_info,
                                    &cpu_model,
                                    &gpu_metrics,
                                    &ram_info,
                                );
                                copy_to_clipboard(&text);
                                copy_feedback_until = Some(Instant::now() + Duration::from_secs(2));
                            }
                        }
                    } else if current_tab == 1 {
                        let num_procs = processes
                            .iter()
                            .filter(|p| advanced_view || p.rss_kb > 0)
                            .filter(|p| {
                                search_query.is_empty()
                                    || fuzzy_match(&search_query, &p.name)
                                    || fuzzy_match(&search_query, &p.pid.to_string())
                                    || fuzzy_match(&search_query, &p.user)
                            })
                            .count();
                        match mouse_event.kind {
                            MouseEventKind::ScrollDown => {
                                let i = match table_state.selected() {
                                    Some(i) => (i + 3).min(num_procs.saturating_sub(1)),
                                    None => 0,
                                };
                                table_state.select(Some(i));
                            }
                            MouseEventKind::ScrollUp => {
                                let i = match table_state.selected() {
                                    Some(i) => i.saturating_sub(3),
                                    None => 0,
                                };
                                table_state.select(Some(i));
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                let my = mouse_event.row;
                                let header_y = table_area.y + 1;
                                if my == header_y {
                                    let mx = mouse_event.column;
                                    let (col_widths, cols): (Vec<u16>, &[ProcessSortColumn]) =
                                        if advanced_view {
                                            let base_w = 8 + 10 + 7 + 8 + 8 + 10 + 8 + 10 + 23 + 23;
                                            let name_w =
                                                table_area.width.saturating_sub(base_w).max(10);
                                            (
                                                vec![8, 10, name_w, 7, 8, 8, 10, 8, 10, 23, 23],
                                                &ADVANCED_SORT_COLUMNS,
                                            )
                                        } else {
                                            let base_w = 8 + 8 + 10 + 8 + 10 + 23 + 23;
                                            let name_w =
                                                table_area.width.saturating_sub(base_w).max(10);
                                            (
                                                vec![8, name_w, 8, 10, 8, 10, 23, 23],
                                                &NORMAL_SORT_COLUMNS,
                                            )
                                        };

                                    let mut current_x = table_area.x + 1;
                                    for (i, &w) in col_widths.iter().enumerate() {
                                        if mx >= current_x && mx < current_x + w {
                                            if current_sort_col == cols[i] {
                                                sort_ascending = !sort_ascending;
                                            } else {
                                                current_sort_col = cols[i];
                                            }
                                            sort_processes(
                                                &mut processes,
                                                current_sort_col,
                                                sort_ascending,
                                            );
                                            break;
                                        }
                                        current_x += w;
                                    }
                                } else if my > header_y + 1 && my < table_area.bottom() {
                                    let clicked_row = (my - (header_y + 2)) as usize;
                                    let proc_idx = table_state.offset() + clicked_row;
                                    if proc_idx < num_procs {
                                        table_state.select(Some(proc_idx));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        if !is_paused && last_tick.elapsed() >= tick_rate {
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

            if !prev_ticks.is_empty() && !curr_ticks.is_empty() {
                global_usage = calculate_usage(&prev_ticks[0], &curr_ticks[0]);
                core_usages.clear();
                for i in 1..curr_ticks.len().min(prev_ticks.len()) {
                    core_usages.push(calculate_usage(&prev_ticks[i], &curr_ticks[i]));
                }
            }

            gpu_metrics = read_gpu_metrics();
            let v_pct = if gpu_metrics.vram_total_mb > 0 {
                (gpu_metrics.vram_used_mb as f64 / gpu_metrics.vram_total_mb as f64) * 100.0
            } else {
                0.0
            };

            net_ifaces = read_network_interfaces(&mut prev_net, dt);
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
            docker_info = read_docker_storage();
            let d_read_pct = io_gradient_pct(disk_io.read_speed);
            let d_write_pct = io_gradient_pct(disk_io.write_speed);

            for i in 0..99 {
                cpu_history[i] = cpu_history[i + 1];
                mem_history[i] = mem_history[i + 1];
                gpu_history[i] = gpu_history[i + 1];
                gpu_vram_history[i] = gpu_vram_history[i + 1];
                net_rx_history[i] = net_rx_history[i + 1];
                net_tx_history[i] = net_tx_history[i + 1];
                disk_read_history[i] = disk_read_history[i + 1];
                disk_write_history[i] = disk_write_history[i + 1];
            }
            cpu_history[99] = Some(global_usage);
            mem_history[99] = Some(m_pct);
            gpu_history[99] = Some(gpu_metrics.utilization_pct);
            gpu_vram_history[99] = Some(v_pct);
            net_rx_history[99] = Some(rx_pct);
            net_tx_history[99] = Some(tx_pct);
            disk_read_history[99] = Some(d_read_pct);
            disk_write_history[99] = Some(d_write_pct);

            last_tick = Instant::now();
        }
    }

    Ok(())
}
