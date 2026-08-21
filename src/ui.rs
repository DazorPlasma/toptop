//! User interface rendering engine.
//!
//! Provides Ratatui terminal UI rendering components, including high-resolution
//! 2x4 sub-pixel Unicode Braille gradient historical graphs, tables, gauges,
//! and dashboard cards for all 6 monitor tabs.

use std::collections::{HashMap, HashSet};

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Row, Table, TableState, Tabs},
};

use crate::{
    process::{
        ProcessInfo, ProcessKillConfirmation, ProcessSortColumn, group_processes_for_simple_view,
        matches_process_search,
    },
    system::{
        BatteryInfo, DiskIoInfo, GpuMetrics, MemoryMetrics, MountInfo, NetConnectionInfo,
        NetInterfaceInfo, PackageStorageCategory, SystemGeneralInfo,
    },
    theme::{darken_color, gradient_color, io_gradient_pct, process_cpu_color},
    utils::{
        format_bytes_dyn, format_freq, format_percent, format_uptime, is_lts_or_latest_kernel,
        is_ram_under_8gb, wrap_text,
    },
};

/// Computes the 8-bit Unicode Braille dot bitmask for a given sub-pixel cell coordinate.
///
/// # Arguments
/// * `sub_x` - Horizontal sub-pixel index within character cell (0 or 1).
/// * `sub_y` - Vertical sub-pixel index within character cell (0, 1, 2, or 3).
///
/// # Returns
/// A bitmask corresponding to the Braille dot position.
pub fn braille_dot_mask(sub_x: usize, sub_y: usize) -> u8 {
    match (sub_x, sub_y) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x80,
        _ => 0,
    }
}

/// Converts an 8-bit Braille dot bitmask into the corresponding Unicode Braille character.
///
/// # Arguments
/// * `mask` - 8-bit dot pattern mask.
///
/// # Returns
/// Unicode character in the range U+2800 to U+28FF.
pub fn braille_char(mask: u8) -> char {
    if mask == 0 {
        ' '
    } else {
        std::char::from_u32(0x2800 + mask as u32).unwrap_or(' ')
    }
}

/// Renders a high-resolution 2x4 sub-pixel Braille historical trend graph with a gradient fill.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the graph widget.
/// * `title` - Left-aligned header title text.
/// * `bottom_center_title` - Optional bottom-centered badge text and highlight color.
/// * `right_title` - Optional right-aligned sub-header text.
/// * `border_color` - Border line color.
/// * `history` - Slice of optional data samples (0.0 to 100.0) where `None` indicates uncollected startup state.
pub fn render_gradient_chart(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    bottom_center_title: Option<(&str, Color)>,
    right_title: Option<&str>,
    border_color: Color,
    history: &[Option<f64>],
) {
    let mut block = Block::default()
        .title(title.fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color));

    if let Some((bct, color)) = bottom_center_title
        && !bct.is_empty()
    {
        block = block.title_bottom(
            Line::from(format!(" {} ", bct).fg(color).bold())
                .alignment(ratatui::layout::Alignment::Center),
        );
    }

    if let Some(rt) = right_title
        && !rt.is_empty()
    {
        block = block.title(
            Line::from(format!(" {} ", rt).fg(Color::Rgb(170, 170, 170)))
                .alignment(ratatui::layout::Alignment::Right),
        );
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 3 {
        return;
    }

    let y_axis_width: u16 = 5;
    let x_axis_height: u16 = 1;

    let graph_x = inner.x + y_axis_width;
    let graph_y = inner.y;
    let graph_w = inner.width.saturating_sub(y_axis_width);
    let graph_h = inner.height.saturating_sub(x_axis_height);

    if graph_w == 0 || graph_h == 0 {
        return;
    }

    let axis_style = Style::default().fg(Color::Rgb(170, 170, 170));

    for row in 0..graph_h {
        let screen_y = graph_y + row;
        let label = if row == 0 {
            "100%│"
        } else if row == graph_h / 2 {
            " 50%│"
        } else if row == graph_h.saturating_sub(1) {
            "  0%│"
        } else {
            "    │"
        };
        for (i, ch) in label.chars().enumerate() {
            let col = inner.x + i as u16;
            if col < graph_x {
                frame.buffer_mut()[(col, screen_y)]
                    .set_char(ch)
                    .set_style(axis_style);
            }
        }
    }

    let x_line_y = graph_y + graph_h;
    if x_line_y < inner.bottom() {
        for (i, ch) in "    └".chars().enumerate() {
            let col = inner.x + i as u16;
            if col < graph_x {
                frame.buffer_mut()[(col, x_line_y)]
                    .set_char(ch)
                    .set_style(axis_style);
            }
        }

        for col in graph_x..inner.right() {
            frame.buffer_mut()[(col, x_line_y)]
                .set_char('─')
                .set_style(axis_style);
        }

        let older = "Older";
        for (i, ch) in older.chars().enumerate() {
            let col = graph_x + i as u16;
            if col < inner.right() {
                frame.buffer_mut()[(col, x_line_y)]
                    .set_char(ch)
                    .set_style(axis_style);
            }
        }

        let newer = "Newer";
        let newer_start = inner.right().saturating_sub(newer.len() as u16);
        for (i, ch) in newer.chars().enumerate() {
            let col = newer_start + i as u16;
            if col >= graph_x && col < inner.right() {
                frame.buffer_mut()[(col, x_line_y)]
                    .set_char(ch)
                    .set_style(axis_style);
            }
        }
    }

    let pw = (graph_w as usize) * 2;
    let ph = (graph_h as usize) * 4;

    let mut cell_masks = vec![vec![0u8; graph_h as usize]; graph_w as usize];
    let mut cell_colors = vec![vec![Color::Rgb(0, 255, 0); graph_h as usize]; graph_w as usize];

    let mut prev_py: Option<usize> = None;

    for px in 0..pw {
        let hist_idx = (px as f64) / (pw.max(1) as f64) * (history.len().saturating_sub(1) as f64);
        let idx0 = (hist_idx.floor() as usize).min(history.len().saturating_sub(1));
        let idx1 = (idx0 + 1).min(history.len().saturating_sub(1));
        let frac = hist_idx - idx0 as f64;

        let val_opt = match (history[idx0], history[idx1]) {
            (Some(v0), Some(v1)) => Some(v0 * (1.0 - frac) + v1 * frac),
            (None, Some(v1)) if frac >= 0.5 => Some(v1),
            (Some(v0), None) if frac < 0.5 => Some(v0),
            _ => None,
        };

        if let Some(val) = val_opt {
            let val_clamped = val.clamp(0.0, 100.0);
            let py = ((val_clamped / 100.0) * (ph.saturating_sub(1) as f64)).round() as usize;

            let y_start = prev_py.unwrap_or(py).min(py);
            let y_end = prev_py.unwrap_or(py).max(py);

            for y in y_start..=y_end {
                if y < ph {
                    let cell_x = px / 2;
                    let cell_y = (graph_h as usize - 1) - (y / 4);
                    let sub_x = px % 2;
                    let sub_y = 3 - (y % 4);

                    if cell_x < graph_w as usize && cell_y < graph_h as usize {
                        cell_masks[cell_x][cell_y] |= braille_dot_mask(sub_x, sub_y);
                        let pct = (y as f64) / (ph.max(1) as f64) * 100.0;
                        cell_colors[cell_x][cell_y] = gradient_color(pct);
                    }
                }
            }

            prev_py = Some(py);
        } else {
            prev_py = None;
        }
    }

    for cx in 0..graph_w as usize {
        for cy in 0..graph_h as usize {
            let mask = cell_masks[cx][cy];
            if mask != 0 {
                let col = graph_x + cx as u16;
                let row = graph_y + cy as u16;
                if col < inner.right() && row < graph_y + graph_h {
                    let ch = braille_char(mask);
                    frame.buffer_mut()[(col, row)]
                        .set_char(ch)
                        .set_style(Style::default().fg(cell_colors[cx][cy]));
                }
            }
        }
    }
}

/// Formats a process command string into styled Ratatui Spans, highlighting the binary/app
/// and dimming command line option flags and arguments (`-f`, `--option`, `value`).
///
/// # Arguments
/// * `cmd` - Process name or command line invocation.
/// * `is_selected` - Whether the current table row is cursor selected.
///
/// # Returns
/// A Ratatui `Line` containing styled text spans.
pub fn format_command_spans<'a>(cmd: &'a str, is_selected: bool) -> Line<'a> {
    let mut spans = Vec::new();
    let tokens: Vec<&'a str> = cmd.split_whitespace().collect();
    let mut in_args = false;

    for (i, &tok) in tokens.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }

        if tok == "▶" || tok == "▼" {
            let style = if is_selected {
                Style::default().fg(Color::Rgb(255, 255, 255)).bold()
            } else {
                Style::default().fg(Color::Rgb(240, 240, 240)).bold()
            };
            spans.push(Span::styled(tok, style));
            continue;
        }

        if tok == "├─" || tok == "└─" {
            let style = if is_selected {
                Style::default().fg(Color::Rgb(180, 180, 180))
            } else {
                Style::default().fg(Color::Rgb(120, 120, 120))
            };
            spans.push(Span::styled(tok, style));
            continue;
        }

        if tok.starts_with('-') {
            in_args = true;
        }

        let style = if in_args {
            if is_selected {
                Style::default().fg(Color::Rgb(160, 160, 160))
            } else {
                Style::default().fg(Color::Rgb(110, 110, 110))
            }
        } else if is_selected {
            Style::default().fg(Color::Rgb(255, 255, 255)).bold()
        } else {
            Style::default().fg(Color::Rgb(240, 240, 240)).bold()
        };
        spans.push(Span::styled(tok, style));
    }

    Line::from(spans)
}

/// Renders the Process Manager (Tab 2) interactive sorting table.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the table.
/// * `processes` - Current snapshot of system processes.
/// * `advanced_view` - Whether advanced view (all columns, kernel threads, raw paths) is enabled.
/// * `expanded_groups` - Set of group names currently expanded in tree view.
/// * `search_query` - Current user search filter text.
/// * `is_searching` - Whether the search query input box is active.
/// * `current_sort_col` - Currently selected sort column.
/// * `sort_ascending` - Sort order direction.
/// * `total_mem` - Total system memory in megabytes for percentage calculations.
/// * `num_cores` - Logical CPU core count for gradient scaling.
/// * `table_state` - Mutable Ratatui table state maintaining cursor selection and scroll offset.
#[allow(clippy::too_many_arguments)]
pub fn render_process_tab(
    frame: &mut Frame,
    area: Rect,
    processes: &[ProcessInfo],
    advanced_view: bool,
    expanded_groups: &HashSet<String>,
    search_query: &str,
    is_searching: bool,
    current_sort_col: ProcessSortColumn,
    sort_ascending: bool,
    total_mem: u64,
    num_cores: usize,
    table_state: &mut TableState,
) {
    let grouped_storage;
    let base_processes: &[ProcessInfo] = if advanced_view {
        processes
    } else {
        grouped_storage = group_processes_for_simple_view(
            processes,
            expanded_groups,
            current_sort_col,
            sort_ascending,
            search_query,
        );
        &grouped_storage
    };

    let proc_map: HashMap<u32, &ProcessInfo> = processes.iter().map(|p| (p.pid, p)).collect();

    let displayed_processes: Vec<&ProcessInfo> = base_processes
        .iter()
        .filter(|p| advanced_view || p.rss_kb > 0)
        .filter(|p| matches_process_search(p, search_query, Some(&proc_map)))
        .collect();

    let show_pid = area.width >= 150;

    let (header_titles, constraints): (Vec<(&str, ProcessSortColumn)>, Vec<Constraint>) =
        if advanced_view {
            let mut titles = Vec::with_capacity(11);
            let mut cons = Vec::with_capacity(11);
            if show_pid {
                titles.push(("PID", ProcessSortColumn::Pid));
                cons.push(Constraint::Length(8));
            }
            titles.push(("User", ProcessSortColumn::User));
            cons.push(Constraint::Length(10));
            titles.push(("Name", ProcessSortColumn::Name));
            cons.push(Constraint::Fill(1));
            titles.push(("State", ProcessSortColumn::State));
            cons.push(Constraint::Length(7));
            titles.push(("Threads", ProcessSortColumn::Threads));
            cons.push(Constraint::Length(8));
            titles.push(("CPU", ProcessSortColumn::Cpu));
            cons.push(Constraint::Length(5));
            titles.push(("RAM", ProcessSortColumn::Mem));
            cons.push(Constraint::Length(10));
            titles.push(("GPU", ProcessSortColumn::Gpu));
            cons.push(Constraint::Length(5));
            titles.push(("VRAM", ProcessSortColumn::GpuMem));
            cons.push(Constraint::Length(9));
            titles.push(("IO", ProcessSortColumn::Io));
            cons.push(Constraint::Length(21));
            titles.push(("Net", ProcessSortColumn::Net));
            cons.push(Constraint::Length(21));
            (titles, cons)
        } else {
            let mut titles = Vec::with_capacity(8);
            let mut cons = Vec::with_capacity(8);
            if show_pid {
                titles.push(("PID", ProcessSortColumn::Pid));
                cons.push(Constraint::Length(8));
            }
            titles.push(("Name", ProcessSortColumn::Name));
            cons.push(Constraint::Fill(1));
            titles.push(("CPU", ProcessSortColumn::Cpu));
            cons.push(Constraint::Length(5));
            titles.push(("RAM", ProcessSortColumn::Mem));
            cons.push(Constraint::Length(10));
            titles.push(("GPU", ProcessSortColumn::Gpu));
            cons.push(Constraint::Length(5));
            titles.push(("VRAM", ProcessSortColumn::GpuMem));
            cons.push(Constraint::Length(9));
            titles.push(("IO", ProcessSortColumn::Io));
            cons.push(Constraint::Length(21));
            titles.push(("Net", ProcessSortColumn::Net));
            cons.push(Constraint::Length(21));
            (titles, cons)
        };

    let header_cells = header_titles.iter().map(|&(title, col)| {
        if col == current_sort_col {
            let (arrow, color) = if sort_ascending {
                ("▲", Color::Rgb(0, 255, 255))
            } else {
                ("▼", Color::Rgb(255, 255, 0))
            };
            Cell::from(format!("{} {}", title, arrow)).style(Style::default().fg(color).bold())
        } else {
            Cell::from(title).style(Style::default().fg(Color::Rgb(255, 255, 255)))
        }
    });
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let selected_idx = table_state.selected();
    let rows = displayed_processes.iter().enumerate().map(|(idx, p)| {
        let is_selected = selected_idx == Some(idx);
        let dim_factor = if is_selected { 1.0 } else { 0.55 };

        let text_color = if is_selected {
            Color::Rgb(255, 255, 255)
        } else {
            Color::Rgb(130, 130, 130)
        };

        let total_mem_kb = (total_mem as f64) * 1024.0;
        let mem_pct = if total_mem_kb > 0.0 {
            (p.rss_kb as f64 / total_mem_kb) * 100.0
        } else {
            0.0
        };

        let gpu_mem_pct = (p.gpu_mem_kb as f64 / (8.0 * 1024.0 * 1024.0) * 100.0).clamp(0.0, 100.0);

        let raw_cpu_color = if p.cpu_percent <= 0.0 {
            Color::Rgb(0, 85, 0)
        } else {
            process_cpu_color(p.cpu_percent, num_cores)
        };
        let cpu_color = darken_color(raw_cpu_color, dim_factor);

        let raw_mem_color = if p.rss_kb == 0 {
            Color::Rgb(0, 85, 0)
        } else {
            gradient_color(mem_pct * 2.0)
        };
        let mem_color = darken_color(raw_mem_color, dim_factor);

        let raw_gpu_color = if p.gpu_percent <= 0.0 {
            Color::Rgb(0, 85, 0)
        } else {
            gradient_color(p.gpu_percent)
        };
        let gpu_color = darken_color(raw_gpu_color, dim_factor);

        let raw_gpu_mem_color = if p.gpu_mem_kb == 0 {
            Color::Rgb(0, 85, 0)
        } else {
            gradient_color(gpu_mem_pct * 2.0)
        };
        let gpu_mem_color = darken_color(raw_gpu_mem_color, dim_factor);

        let io_max = p.read_speed.max(p.write_speed);
        let raw_io_color = if io_max <= 0.0 {
            Color::Rgb(0, 85, 0)
        } else {
            gradient_color(io_gradient_pct(io_max))
        };
        let io_color = darken_color(raw_io_color, dim_factor);
        let io_text = format!(
            "{} ↑ / {} ↓",
            format_bytes_dyn(p.read_speed),
            format_bytes_dyn(p.write_speed)
        );

        let net_max = p.net_rx_speed.max(p.net_tx_speed);
        let raw_net_color = if net_max <= 0.0 {
            Color::Rgb(0, 85, 0)
        } else {
            gradient_color(io_gradient_pct(net_max))
        };
        let net_color = darken_color(raw_net_color, dim_factor);
        let net_text = format!(
            "{} ↓ / {} ↑",
            format_bytes_dyn(p.net_rx_speed),
            format_bytes_dyn(p.net_tx_speed)
        );

        let mut cells = Vec::with_capacity(11);
        if show_pid {
            cells.push(Cell::from(p.pid.to_string()).style(Style::default().fg(text_color)));
        }
        if advanced_view {
            cells.push(Cell::from(p.user.clone()).style(Style::default().fg(text_color)));
            cells.push(Cell::from(p.name.clone()).style(Style::default().fg(text_color)));
            cells.push(Cell::from(p.state.clone()).style(Style::default().fg(text_color)));
            cells.push(Cell::from(p.threads.to_string()).style(Style::default().fg(text_color)));
            cells.push(
                Cell::from(format_percent(p.cpu_percent)).style(Style::default().fg(cpu_color)),
            );
            cells.push(
                Cell::from(format_bytes_dyn((p.rss_kb * 1024) as f64))
                    .style(Style::default().fg(mem_color)),
            );
            cells.push(
                Cell::from(format_percent(p.gpu_percent)).style(Style::default().fg(gpu_color)),
            );
            cells.push(
                Cell::from(format_bytes_dyn((p.gpu_mem_kb * 1024) as f64))
                    .style(Style::default().fg(gpu_mem_color)),
            );
            cells.push(Cell::from(io_text).style(Style::default().fg(io_color)));
            cells.push(Cell::from(net_text).style(Style::default().fg(net_color)));
        } else {
            cells.push(Cell::from(format_command_spans(&p.name, is_selected)));
            cells.push(
                Cell::from(format_percent(p.cpu_percent)).style(Style::default().fg(cpu_color)),
            );
            cells.push(
                Cell::from(format_bytes_dyn((p.rss_kb * 1024) as f64))
                    .style(Style::default().fg(mem_color)),
            );
            cells.push(
                Cell::from(format_percent(p.gpu_percent)).style(Style::default().fg(gpu_color)),
            );
            cells.push(
                Cell::from(format_bytes_dyn((p.gpu_mem_kb * 1024) as f64))
                    .style(Style::default().fg(gpu_mem_color)),
            );
            cells.push(Cell::from(io_text).style(Style::default().fg(io_color)));
            cells.push(Cell::from(net_text).style(Style::default().fg(net_color)));
        }
        Row::new(cells).height(1)
    });

    let table_title = if is_searching {
        format!(
            " Search: {}_ (Enter to finish, Esc to cancel) ",
            search_query
        )
    } else if !search_query.is_empty() {
        let mode_str = if advanced_view { "Advanced, " } else { "" };
        format!(
            " Processes [{}Filter: \"{}\" - {} matches] ('/' Search, Esc Clear, ←/→ Sort, 'r' Order, 'c' Term, 't' Kill, ↑/↓/g/G Select) ",
            mode_str,
            search_query,
            displayed_processes.len()
        )
    } else if advanced_view {
        " Processes [Advanced] ('/' Search, 'r' Order, 'c' Term, 't' Kill, 'a' Normal View) "
            .to_string()
    } else {
        " Processes ('/' Search, 'r' Order, 'c' Term, 't' Kill, 'a' Advanced View) "
            .to_string()
    }.fg(Color::Rgb(170, 170, 170));

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                .title(table_title),
        )
        .row_highlight_style(Style::default().bg(Color::Rgb(45, 45, 45)));

    frame.render_stateful_widget(table, area, table_state);
}

/// Renders the CPU & RAM (Tab 3) historical Braille charts, frequency/temperature gauges, and per-core grid.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the tab.
/// * `cpu_history` - Historical CPU usage samples for Braille graphing.
/// * `core_usages` - Current per-core load percentages.
/// * `cpu_cur_mhz` - Current CPU clock frequency in MHz.
/// * `cpu_min_mhz` - Minimum CPU clock frequency in MHz.
/// * `cpu_max_mhz` - Maximum CPU clock frequency in MHz.
/// * `cpu_temp` - CPU temperature in degrees Celsius.
/// * `cpu_model` - Marketing CPU model identifier string.
/// * `mem` - Memory utilization metrics (RAM and swap).
/// * `mem_history` - Historical RAM usage samples for Braille graphing.
/// * `swap_history` - Historical Swap usage samples for Braille graphing.
/// * `ram_info` - DMI memory capacity, speed, and DDR generation string.
#[allow(clippy::too_many_arguments)]
pub fn render_cpu_ram_tab(
    frame: &mut Frame,
    area: Rect,
    cpu_history: &[Option<f64>],
    core_usages: &[f64],
    cpu_cur_mhz: f64,
    cpu_min_mhz: f64,
    cpu_max_mhz: f64,
    cpu_temp: u32,
    cpu_model: &str,
    mem: &MemoryMetrics,
    mem_history: &[Option<f64>],
    swap_history: &[Option<f64>],
    ram_info: &str,
) {
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let num_cores = core_usages.len();
    let available_rows = body_chunks[0].height.saturating_sub(2) as usize;
    let needs_two_cols = available_rows > 0 && num_cores > available_rows;

    let cores_constraint = if needs_two_cols {
        Constraint::Percentage(36)
    } else {
        Constraint::Percentage(20)
    };

    let cpu_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), cores_constraint, Constraint::Length(9)])
        .split(body_chunks[0]);

    let cpu_freq_pct = if cpu_max_mhz > cpu_min_mhz {
        ((cpu_cur_mhz - cpu_min_mhz) / (cpu_max_mhz - cpu_min_mhz) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let cpu_freq_label = format_freq(cpu_cur_mhz);
    let cpu_freq_color = gradient_color(cpu_freq_pct);

    render_gradient_chart(
        frame,
        cpu_chunks[0],
        "CPU History",
        None,
        Some(cpu_model),
        Color::Rgb(60, 60, 60),
        cpu_history,
    );

    let cores_block = Block::default()
        .title(" Cores ".fg(Color::Rgb(170, 170, 170)))
        .title(
            Line::from(format!(" {} ", cpu_freq_label).fg(cpu_freq_color))
                .alignment(ratatui::layout::Alignment::Right),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let cores_inner = cores_block.inner(cpu_chunks[1]);
    frame.render_widget(cores_block, cpu_chunks[1]);

    if cores_inner.width > 0 && cores_inner.height > 0 {
        let is_two_col = needs_two_cols && cores_inner.width >= 32;
        let col_w = if is_two_col {
            (cores_inner.width.saturating_sub(1)) / 2
        } else {
            cores_inner.width
        };

        for (i, &usage) in core_usages.iter().enumerate() {
            let (col_idx, row_idx) = if is_two_col { (i % 2, i / 2) } else { (0, i) };

            let row_y = cores_inner.y + row_idx as u16;
            if row_y >= cores_inner.bottom() {
                break;
            }

            let start_x = if col_idx == 0 {
                cores_inner.x
            } else {
                cores_inner.x + col_w + 1
            };
            let max_x = if col_idx == 0 && is_two_col {
                cores_inner.x + col_w
            } else {
                cores_inner.right()
            };

            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: start_x,
                    y: row_y,
                    width: max_x.saturating_sub(start_x),
                    height: 1,
                },
                &format!("C{:<2}:", i),
                &format!("{:.0}%", usage),
                usage,
            );
        }
    }

    let cpu_temp_block = Block::default()
        .title(" Temp ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let cpu_temp_inner = cpu_temp_block.inner(cpu_chunks[2]);
    frame.render_widget(cpu_temp_block, cpu_chunks[2]);

    if cpu_temp_inner.height > 0 && cpu_temp_inner.width > 0 {
        let temp_pct = ((cpu_temp as f64 - 25.0) / 75.0 * 100.0).clamp(0.0, 100.0);
        let temp_color = gradient_color(temp_pct);

        let temp_str = format!("{}°C", cpu_temp);
        let str_len = temp_str.chars().count() as u16;
        let str_start = cpu_temp_inner.x + (cpu_temp_inner.width.saturating_sub(str_len)) / 2;

        for (idx, ch) in temp_str.chars().enumerate() {
            let col = str_start + idx as u16;
            if col < cpu_temp_inner.right() {
                frame.buffer_mut()[(col, cpu_temp_inner.y)]
                    .set_char(ch)
                    .set_style(Style::default().fg(temp_color).bold());
            }
        }

        if cpu_temp_inner.height > 2 {
            let bar_height = cpu_temp_inner.height - 2;
            let filled_rows = ((bar_height as f64) * (temp_pct / 100.0)).round() as u16;
            let bar_width = 3.min(cpu_temp_inner.width);
            let bar_x_start =
                cpu_temp_inner.x + (cpu_temp_inner.width.saturating_sub(bar_width)) / 2;

            for r in 0..bar_height {
                let row_y = cpu_temp_inner.bottom() - 1 - r;
                let row_pct = (r as f64 + 0.5) / (bar_height as f64) * 100.0;
                let is_filled = r < filled_rows;
                let color = gradient_color(row_pct);

                for c in 0..bar_width {
                    let col = bar_x_start + c;
                    if col < cpu_temp_inner.right() && row_y < cpu_temp_inner.bottom() {
                        if is_filled {
                            frame.buffer_mut()[(col, row_y)]
                                .set_char('█')
                                .set_style(Style::default().fg(color));
                        } else {
                            frame.buffer_mut()[(col, row_y)]
                                .set_char('░')
                                .set_style(Style::default().fg(Color::Rgb(50, 50, 50)));
                        }
                    }
                }
            }
        }
    }

    let ram_swap_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(body_chunks[1]);

    let ram_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(ram_swap_cols[0]);

    let swap_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(ram_swap_cols[1]);

    let mem_percent_f64 = if mem.total_mem_mb > 0 {
        (mem.used_mem_mb as f64 / mem.total_mem_mb as f64) * 100.0
    } else {
        0.0
    };

    let swap_pct = if mem.total_swap_mb > 0 {
        (mem.used_swap_mb as f64 / mem.total_swap_mb as f64) * 100.0
    } else {
        0.0
    };

    let swap_label = if mem.total_swap_mb > 0 {
        format!("{:.0}%", swap_pct)
    } else {
        "None".to_string()
    };

    render_gradient_chart(
        frame,
        ram_layout[0],
        "Memory History",
        None,
        Some(ram_info),
        Color::Rgb(60, 60, 60),
        mem_history,
    );

    render_gradient_chart(
        frame,
        swap_layout[0],
        "Swap History",
        None,
        Some(&swap_label),
        Color::Rgb(60, 60, 60),
        swap_history,
    );

    let mem_block = Block::default()
        .title(" Memory Usage ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let mem_inner = mem_block.inner(ram_layout[1]);
    frame.render_widget(mem_block, ram_layout[1]);
    if mem_inner.height > 0 && mem_inner.width > 0 {
        draw_centered_gradient_bar(
            frame.buffer_mut(),
            mem_inner,
            &format!("{} MB / {} MB", mem.used_mem_mb, mem.total_mem_mb),
            mem_percent_f64,
        );
    }

    let swap_block = Block::default()
        .title(" Swap Usage ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let swap_inner = swap_block.inner(swap_layout[1]);
    frame.render_widget(swap_block, swap_layout[1]);

    if swap_inner.height > 0 && swap_inner.width > 0 {
        let label = if mem.total_swap_mb > 0 {
            format!("{} MB / {} MB", mem.used_swap_mb, mem.total_swap_mb)
        } else {
            "0 MB / 0 MB (No Swap Configured)".to_string()
        };
        draw_centered_gradient_bar(frame.buffer_mut(), swap_inner, &label, swap_pct);
    }
}

/// Draws a single-line centered text label over a horizontal gradient progress bar.
fn draw_centered_gradient_bar(
    buf: &mut ratatui::buffer::Buffer,
    inner: Rect,
    label: &str,
    pct: f64,
) {
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let total_bar = inner.width;
    let filled_len = ((total_bar as f64) * (pct.clamp(0.0, 100.0) / 100.0)).round() as u16;
    let label_chars: Vec<char> = label.chars().collect();
    let label_start = (total_bar.saturating_sub(label_chars.len() as u16)) / 2;

    for c in 0..total_bar {
        let col = inner.x + c;
        let row = inner.y;
        let p = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
        let is_filled = c < filled_len;
        let bar_color = gradient_color(p);

        if c >= label_start && (c - label_start) < label_chars.len() as u16 {
            let ch = label_chars[(c - label_start) as usize];
            if is_filled {
                buf[(col, row)].set_char(ch).set_style(
                    Style::default()
                        .fg(Color::Rgb(0, 0, 0))
                        .bg(bar_color)
                        .bold(),
                );
            } else {
                buf[(col, row)].set_char(ch).set_style(
                    Style::default()
                        .fg(Color::Rgb(170, 170, 170))
                        .bg(Color::Rgb(30, 30, 30)),
                );
            }
        } else if is_filled {
            buf[(col, row)]
                .set_char('█')
                .set_style(Style::default().fg(bar_color));
        } else {
            buf[(col, row)]
                .set_char(' ')
                .set_style(Style::default().bg(Color::Rgb(30, 30, 30)));
        }
    }
}

/// Checks whether the GPU model name exceeds available chart title width in side-by-side mode,
/// causing title text overlap with "GPU Utilization".
///
/// # Arguments
/// * `area` - Target bounding box for the GPU tab.
/// * `gpu_metrics` - Current GPU metrics containing name and VRAM info.
///
/// # Returns
/// `true` if GPU utilization and VRAM history charts should collapse into a tabbed sub-view.
pub fn is_gpu_overflow(area: Rect, gpu_metrics: &GpuMetrics) -> bool {
    let top_w = (area.width * 60) / 100;
    let graph_w = top_w / 2;
    let vram_gb = ((gpu_metrics.vram_total_mb as f64) / 1024.0).round() as u64;
    let gpu_label_len = if vram_gb > 0 {
        gpu_metrics.name.chars().count() + format!(" ({}GB)", vram_gb).chars().count()
    } else {
        gpu_metrics.name.chars().count()
    };
    let min_needed = (15 + gpu_label_len + 4) as u16;
    graph_w < min_needed
}

/// Renders the GPU (Tab 4) utilization graph, VRAM graph, core/memory clocks, power, fans, and temperatures.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the tab.
/// * `gpu_metrics` - Current GPU metrics and status.
/// * `gpu_history` - Historical GPU core busy percentages for Braille graphing.
/// * `gpu_vram_history` - Historical GPU VRAM consumption for Braille graphing.
/// * `sub_tab` - Active sub-tab index (0: GPU Utilization, 1: VRAM History) when in tabbed view.
pub fn render_gpu_tab(
    frame: &mut Frame,
    area: Rect,
    gpu_metrics: &GpuMetrics,
    gpu_history: &[Option<f64>],
    gpu_vram_history: &[Option<f64>],
    sub_tab: usize,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(body_chunks[0]);

    let vram_gb = ((gpu_metrics.vram_total_mb as f64) / 1024.0).round() as u64;
    let gpu_label = if vram_gb > 0 {
        format!("{} ({}GB)", gpu_metrics.name, vram_gb)
    } else {
        gpu_metrics.name.clone()
    };

    let gpu_freq_pct = if gpu_metrics.max_mhz > gpu_metrics.min_mhz {
        ((gpu_metrics.cur_mhz - gpu_metrics.min_mhz) / (gpu_metrics.max_mhz - gpu_metrics.min_mhz)
            * 100.0)
            .clamp(0.0, 100.0)
    } else {
        0.0
    };
    let gpu_freq_label = format_freq(gpu_metrics.cur_mhz);
    let gpu_freq_color = gradient_color(gpu_freq_pct);

    let vram_label = format!(
        "{} MB / {} MB",
        gpu_metrics.vram_used_mb, gpu_metrics.vram_total_mb
    );
    let mem_vendor_label = if !gpu_metrics.memory_vendor.is_empty() {
        format!("{} VRAM", gpu_metrics.memory_vendor)
    } else {
        "VRAM".to_string()
    };

    let is_tabbed = is_gpu_overflow(area, gpu_metrics);

    if is_tabbed {
        let sub_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(top_chunks[0]);

        let titles = vec!["GPU Utilization", "VRAM History"];
        let tabs = Tabs::new(titles)
            .style(Style::default().fg(Color::Rgb(170, 170, 170)))
            .select(sub_tab % 2)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                    .title(" View (←/→ switch tab) ".fg(Color::Rgb(170, 170, 170))),
            )
            .highlight_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold())
            .divider("|");

        frame.render_widget(tabs, sub_chunks[0]);

        match sub_tab % 2 {
            0 => {
                render_gradient_chart(
                    frame,
                    sub_chunks[1],
                    "GPU Utilization",
                    Some((&gpu_freq_label, gpu_freq_color)),
                    Some(&gpu_label),
                    Color::Rgb(60, 60, 60),
                    gpu_history,
                );
            }
            1 => {
                render_gradient_chart(
                    frame,
                    sub_chunks[1],
                    "VRAM History",
                    None,
                    Some(&vram_label),
                    Color::Rgb(60, 60, 60),
                    gpu_vram_history,
                );
            }
            _ => {}
        }
    } else {
        let graph_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(top_chunks[0]);

        render_gradient_chart(
            frame,
            graph_chunks[0],
            "GPU Utilization",
            Some((&gpu_freq_label, gpu_freq_color)),
            Some(&gpu_label),
            Color::Rgb(60, 60, 60),
            gpu_history,
        );

        render_gradient_chart(
            frame,
            graph_chunks[1],
            "VRAM History",
            None,
            Some(&vram_label),
            Color::Rgb(60, 60, 60),
            gpu_vram_history,
        );
    }

    // Top Right: Hardware & Telemetry card
    let hw_block = Block::default()
        .title(" Hardware & Telemetry ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let hw_inner = hw_block.inner(top_chunks[1]);
    frame.render_widget(hw_block, top_chunks[1]);

    if hw_inner.height > 0 && hw_inner.width > 0 {
        let mut lines = Vec::new();
        lines.push(format!("Model:    {}", gpu_metrics.name));
        if !gpu_metrics.driver.is_empty() {
            lines.push(format!("Driver:   {}", gpu_metrics.driver));
        }
        if !gpu_metrics.pcie_link.is_empty() {
            lines.push(format!("PCIe Bus: {}", gpu_metrics.pcie_link));
        }
        if gpu_metrics.cur_mhz > 0.0 || gpu_metrics.mem_cur_mhz > 0.0 {
            lines.push(format!(
                "Clocks:   Core: {:.0} MHz | Mem: {:.0} MHz",
                gpu_metrics.cur_mhz, gpu_metrics.mem_cur_mhz
            ));
        }
        if gpu_metrics.voltage_mv > 0 {
            lines.push(format!(
                "Voltage:  {} mV ({:.2} V)",
                gpu_metrics.voltage_mv,
                gpu_metrics.voltage_mv as f64 / 1000.0
            ));
        }

        for (idx, line_str) in lines.iter().enumerate() {
            let row = hw_inner.y + idx as u16;
            if row < hw_inner.bottom() {
                for (c_idx, ch) in line_str.chars().enumerate() {
                    let col = hw_inner.x + c_idx as u16;
                    if col < hw_inner.right() {
                        let color = if idx == 0 {
                            Color::Rgb(220, 220, 220)
                        } else {
                            Color::Rgb(170, 170, 170)
                        };
                        frame.buffer_mut()[(col, row)]
                            .set_char(ch)
                            .set_style(Style::default().fg(color));
                    }
                }
            }
        }
    }

    // Bottom chunks: Left (Memory Gauges), Right (Thermals & Power Gauges)
    let bot_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(body_chunks[1]);

    // Bottom Left: Dedicated VRAM + GTT System RAM
    let mem_constraints = if gpu_metrics.gtt_total_mb > 0 {
        vec![
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
        ]
    } else {
        vec![Constraint::Length(3), Constraint::Min(0)]
    };
    let mem_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(mem_constraints)
        .split(bot_chunks[0]);

    // VRAM Bar
    let vram_pct = if gpu_metrics.vram_total_mb > 0 {
        (gpu_metrics.vram_used_mb as f64 / gpu_metrics.vram_total_mb as f64) * 100.0
    } else {
        0.0
    };
    let vram_title = if !mem_vendor_label.is_empty() {
        format!(" Dedicated VRAM ({}) ", mem_vendor_label)
    } else {
        " Dedicated VRAM ".to_string()
    };
    let vram_block = Block::default()
        .title(vram_title.fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let vram_inner = vram_block.inner(mem_layout[0]);
    frame.render_widget(vram_block, mem_layout[0]);
    if vram_inner.height > 0 && vram_inner.width > 0 {
        draw_centered_gradient_bar(
            frame.buffer_mut(),
            vram_inner,
            &format!(
                "{} MB / {} MB ({:.0}%)",
                gpu_metrics.vram_used_mb, gpu_metrics.vram_total_mb, vram_pct
            ),
            vram_pct,
        );
    }

    // GTT Shared Bar
    if gpu_metrics.gtt_total_mb > 0 && mem_layout.len() > 1 {
        let gtt_pct = (gpu_metrics.gtt_used_mb as f64 / gpu_metrics.gtt_total_mb as f64) * 100.0;
        let gtt_block = Block::default()
            .title(" Shared System RAM (GTT) ".fg(Color::Rgb(170, 170, 170)))
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
        let gtt_inner = gtt_block.inner(mem_layout[1]);
        frame.render_widget(gtt_block, mem_layout[1]);
        if gtt_inner.height > 0 && gtt_inner.width > 0 {
            draw_centered_gradient_bar(
                frame.buffer_mut(),
                gtt_inner,
                &format!(
                    "{} MB / {} MB ({:.0}%)",
                    gpu_metrics.gtt_used_mb, gpu_metrics.gtt_total_mb, gtt_pct
                ),
                gtt_pct,
            );
        }
    }

    // Bottom Right: Thermal Sensors & Power / Fan Bars
    let thermal_block = Block::default()
        .title(" Thermal & Power Sensors ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let thermal_inner = thermal_block.inner(bot_chunks[1]);
    frame.render_widget(thermal_block, bot_chunks[1]);

    if thermal_inner.height > 0 && thermal_inner.width > 0 {
        let mut row_idx = 0;
        if gpu_metrics.temp_edge_c > 0 {
            let pct = ((gpu_metrics.temp_edge_c as f64 - 25.0) / 75.0 * 100.0).clamp(0.0, 100.0);
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: thermal_inner.x,
                    y: thermal_inner.y + row_idx,
                    width: thermal_inner.width,
                    height: 1,
                },
                "Edge Temp:    ",
                &format!("{}°C", gpu_metrics.temp_edge_c),
                pct,
            );
            row_idx += 1;
        }
        if gpu_metrics.temp_junction_c > 0 && thermal_inner.y + row_idx < thermal_inner.bottom() {
            let pct =
                ((gpu_metrics.temp_junction_c as f64 - 25.0) / 85.0 * 100.0).clamp(0.0, 100.0);
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: thermal_inner.x,
                    y: thermal_inner.y + row_idx,
                    width: thermal_inner.width,
                    height: 1,
                },
                "Hotspot Temp: ",
                &format!("{}°C", gpu_metrics.temp_junction_c),
                pct,
            );
            row_idx += 1;
        }
        if gpu_metrics.temp_mem_c > 0 && thermal_inner.y + row_idx < thermal_inner.bottom() {
            let pct = ((gpu_metrics.temp_mem_c as f64 - 25.0) / 80.0 * 100.0).clamp(0.0, 100.0);
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: thermal_inner.x,
                    y: thermal_inner.y + row_idx,
                    width: thermal_inner.width,
                    height: 1,
                },
                "Memory Temp:  ",
                &format!("{}°C", gpu_metrics.temp_mem_c),
                pct,
            );
            row_idx += 1;
        }
        if (gpu_metrics.power_w > 0.0 || gpu_metrics.power_cap_w > 0.0)
            && thermal_inner.y + row_idx < thermal_inner.bottom()
        {
            let cap = if gpu_metrics.power_cap_w > 0.0 {
                gpu_metrics.power_cap_w
            } else {
                200.0
            };
            let pct = (gpu_metrics.power_w / cap * 100.0).clamp(0.0, 100.0);
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: thermal_inner.x,
                    y: thermal_inner.y + row_idx,
                    width: thermal_inner.width,
                    height: 1,
                },
                "Power Draw:   ",
                &format!(
                    "{:.1} W / {:.1} W",
                    gpu_metrics.power_w, gpu_metrics.power_cap_w
                ),
                pct,
            );
            row_idx += 1;
        }
        if thermal_inner.y + row_idx < thermal_inner.bottom() {
            let fan_label = if gpu_metrics.fan_rpm > 0 {
                format!("{} RPM ({:.0}%)", gpu_metrics.fan_rpm, gpu_metrics.fan_pct)
            } else {
                "0 RPM (0%)".to_string()
            };
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: thermal_inner.x,
                    y: thermal_inner.y + row_idx,
                    width: thermal_inner.width,
                    height: 1,
                },
                "Fan Speed:    ",
                &fan_label,
                gpu_metrics.fan_pct as f64,
            );
        }
    }
}

/// Renders the Network (Tab 5) download/upload graphs, primary interface card, and active IP connections table.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the tab.
/// * `net_ifaces` - List of detected network interfaces and throughput rates.
/// * `net_rx_history` - Historical download (RX) speed samples for Braille graphing.
/// * `net_tx_history` - Historical upload (TX) speed samples for Braille graphing.
/// * `connections_res` - Active network connections or permission error message.
/// * `conn_scroll_offset` - Vertical scroll offset for connections list.
pub fn render_network_tab(
    frame: &mut Frame,
    area: Rect,
    net_ifaces: &[NetInterfaceInfo],
    net_rx_history: &[Option<f64>],
    net_tx_history: &[Option<f64>],
    connections_res: &Result<Vec<NetConnectionInfo>, &'static str>,
    conn_scroll_offset: usize,
) {
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(body_chunks[0]);

    // Top-Left: shared by the two graphs (horizontal split)
    let graph_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(top_chunks[0]);

    let primary_iface = net_ifaces
        .iter()
        .find(|i| i.operstate == "up")
        .or_else(|| net_ifaces.first());
    let rx_speed = primary_iface.map(|i| i.rx_speed).unwrap_or(0.0);
    let tx_speed = primary_iface.map(|i| i.tx_speed).unwrap_or(0.0);

    let rx_label = format!("{} ↓/s", format_bytes_dyn(rx_speed));
    let tx_label = format!("{} ↑/s", format_bytes_dyn(tx_speed));

    render_gradient_chart(
        frame,
        graph_chunks[0],
        "Download (RX) History",
        None,
        Some(&rx_label),
        Color::Rgb(60, 60, 60),
        net_rx_history,
    );

    render_gradient_chart(
        frame,
        graph_chunks[1],
        "Upload (TX) History",
        None,
        Some(&tx_label),
        Color::Rgb(60, 60, 60),
        net_tx_history,
    );

    // Top-Right: Interfaces details
    let iface_block = Block::default()
        .title(" Interfaces ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let iface_inner = iface_block.inner(top_chunks[1]);
    frame.render_widget(iface_block, top_chunks[1]);

    if iface_inner.height > 0 && iface_inner.width > 0 {
        let mut lines = Vec::new();
        if let Some(iface) = primary_iface {
            lines.push(format!("Primary:  {}", iface.name));
            lines.push(format!("Status:   {}", iface.operstate));
            lines.push(format!("Speed:    {} Mbps", iface.speed_mbps));
            lines.push(format!("Duplex:   {}", iface.duplex));
            lines.push(format!("MAC:      {}", iface.mac));
            lines.push(format!(
                "Total RX: {}",
                format_bytes_dyn(iface.rx_bytes as f64)
            ));
            lines.push(format!(
                "Total TX: {}",
                format_bytes_dyn(iface.tx_bytes as f64)
            ));
        }
        if net_ifaces.len() > 1 {
            lines.push("".to_string());
            lines.push("Other:".to_string());
            for other in net_ifaces
                .iter()
                .filter(|i| primary_iface.map(|p| p.name.as_str()) != Some(i.name.as_str()))
            {
                lines.push(format!(
                    "• {}: {} (↓ {} / ↑ {})",
                    other.name,
                    other.operstate,
                    format_bytes_dyn(other.rx_speed),
                    format_bytes_dyn(other.tx_speed)
                ));
            }
        }
        for (idx, line_str) in lines.iter().enumerate() {
            let row = iface_inner.y + idx as u16;
            if row < iface_inner.bottom() {
                for (c_idx, ch) in line_str.chars().enumerate() {
                    let col = iface_inner.x + c_idx as u16;
                    if col < iface_inner.right() {
                        let color = if idx == 1
                            && primary_iface.map(|i| i.operstate.as_str()) == Some("up")
                        {
                            Color::Rgb(0, 255, 128)
                        } else {
                            Color::Rgb(170, 170, 170)
                        };
                        frame.buffer_mut()[(col, row)]
                            .set_char(ch)
                            .set_style(Style::default().fg(color));
                    }
                }
            }
        }
    }

    // Bottom: Active Connections Card
    let conn_block = Block::default()
        .title(" Active Connections ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let conn_inner = conn_block.inner(body_chunks[1]);
    frame.render_widget(conn_block, body_chunks[1]);

    if conn_inner.height > 0 && conn_inner.width > 0 {
        match connections_res {
            Err(msg) => {
                let msg_len = msg.chars().count() as u16;
                let msg_x = conn_inner.x + (conn_inner.width.saturating_sub(msg_len)) / 2;
                let msg_y = conn_inner.y + conn_inner.height / 2;
                if msg_y < conn_inner.bottom() {
                    for (c_idx, ch) in msg.chars().enumerate() {
                        let col = msg_x + c_idx as u16;
                        if col < conn_inner.right() {
                            frame.buffer_mut()[(col, msg_y)]
                                .set_char(ch)
                                .set_style(Style::default().fg(Color::Rgb(255, 80, 80)));
                        }
                    }
                }
            }
            Ok(conns) => {
                if conns.is_empty() {
                    let msg = "No active network connections.";
                    let msg_len = msg.chars().count() as u16;
                    let msg_x = conn_inner.x + (conn_inner.width.saturating_sub(msg_len)) / 2;
                    let msg_y = conn_inner.y + conn_inner.height / 2;
                    if msg_y < conn_inner.bottom() {
                        for (c_idx, ch) in msg.chars().enumerate() {
                            let col = msg_x + c_idx as u16;
                            if col < conn_inner.right() {
                                frame.buffer_mut()[(col, msg_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(Color::Rgb(120, 120, 120)));
                            }
                        }
                    }
                } else {
                    let proto_w = 6usize;
                    let pid_w = 8usize;
                    let prog_w = 16usize;
                    let state_w = 14usize;
                    let rem_cols_w = conn_inner.width as usize;
                    let fixed_w = proto_w + pid_w + prog_w + state_w + 5;
                    let local_w = 26.min(rem_cols_w.saturating_sub(fixed_w) / 2);
                    let remote_w = rem_cols_w.saturating_sub(fixed_w + local_w);

                    let header_str = format!(
                        "{:<proto_w$} {:<pid_w$} {:<prog_w$} {:<local_w$} {:<remote_w$} {:<state_w$}",
                        "Proto",
                        "PID",
                        "Program",
                        "Local Address",
                        "Remote Address / Host",
                        "State",
                        proto_w = proto_w,
                        pid_w = pid_w,
                        prog_w = prog_w,
                        local_w = local_w,
                        remote_w = remote_w,
                        state_w = state_w,
                    );
                    for (c_idx, ch) in header_str.chars().enumerate() {
                        let col = conn_inner.x + c_idx as u16;
                        if col < conn_inner.right() {
                            frame.buffer_mut()[(col, conn_inner.y)]
                                .set_char(ch)
                                .set_style(Style::default().fg(Color::Rgb(200, 200, 200)));
                        }
                    }

                    let visible_rows = conn_inner.height.saturating_sub(1) as usize;
                    let max_scroll = conns.len().saturating_sub(visible_rows);
                    let start_idx = conn_scroll_offset.min(max_scroll);
                    for (row_offset, conn) in (1..).zip(conns.iter().skip(start_idx)) {
                        if row_offset >= conn_inner.height {
                            break;
                        }
                        let row_y = conn_inner.y + row_offset;

                        let proto_str = conn.proto;
                        let pid_str = conn
                            .pid
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "-".to_string());
                        let prog_str = conn.process_name.as_deref().unwrap_or("-");
                        let local_str = if conn.local_ip.is_unspecified() {
                            format!("*:{}", conn.local_port)
                        } else if let Some(ref host) = conn.local_host {
                            format!("{}:{}", host, conn.local_port)
                        } else {
                            format!("{}:{}", conn.local_ip, conn.local_port)
                        };
                        let remote_str = if conn.remote_ip.is_unspecified() && conn.remote_port == 0
                        {
                            "*.*".to_string()
                        } else if let Some(ref host) = conn.remote_host {
                            format!("{}:{}", host, conn.remote_port)
                        } else {
                            format!("{}:{}", conn.remote_ip, conn.remote_port)
                        };
                        let state_str = conn.state;

                        let state_color = match state_str {
                            "ESTABLISHED" => Color::Rgb(0, 255, 128),
                            "LISTEN" => Color::Rgb(100, 180, 255),
                            "TIME_WAIT" | "CLOSE_WAIT" | "FIN_WAIT1" | "FIN_WAIT2" => {
                                Color::Rgb(150, 150, 150)
                            }
                            "SYN_SENT" | "SYN_RECV" => Color::Rgb(255, 200, 50),
                            _ => Color::Rgb(170, 170, 170),
                        };

                        let mut cur_col = conn_inner.x;
                        // Proto
                        for ch in format!("{:<proto_w$}", proto_str, proto_w = proto_w).chars() {
                            if cur_col < conn_inner.right() {
                                frame.buffer_mut()[(cur_col, row_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(Color::Rgb(140, 140, 140)));
                                cur_col += 1;
                            }
                        }
                        cur_col += 1;
                        // PID
                        for ch in format!("{:<pid_w$}", pid_str, pid_w = pid_w).chars() {
                            if cur_col < conn_inner.right() {
                                frame.buffer_mut()[(cur_col, row_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(Color::Rgb(160, 160, 160)));
                                cur_col += 1;
                            }
                        }
                        cur_col += 1;
                        // Program
                        let truncated_prog = if prog_str.chars().count() > prog_w {
                            format!(
                                "{}…",
                                prog_str
                                    .chars()
                                    .take(prog_w.saturating_sub(1))
                                    .collect::<String>()
                            )
                        } else {
                            format!("{:<prog_w$}", prog_str, prog_w = prog_w)
                        };
                        for ch in truncated_prog.chars() {
                            if cur_col < conn_inner.right() {
                                frame.buffer_mut()[(cur_col, row_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(Color::Rgb(240, 240, 240)));
                                cur_col += 1;
                            }
                        }
                        cur_col += 1;
                        // Local
                        let truncated_local = if local_str.chars().count() > local_w {
                            format!(
                                "{}…",
                                local_str
                                    .chars()
                                    .take(local_w.saturating_sub(1))
                                    .collect::<String>()
                            )
                        } else {
                            format!("{:<local_w$}", local_str, local_w = local_w)
                        };
                        for ch in truncated_local.chars() {
                            if cur_col < conn_inner.right() {
                                frame.buffer_mut()[(cur_col, row_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(Color::Rgb(180, 180, 180)));
                                cur_col += 1;
                            }
                        }
                        cur_col += 1;
                        // Remote
                        let truncated_remote = if remote_str.chars().count() > remote_w {
                            format!(
                                "{}…",
                                remote_str
                                    .chars()
                                    .take(remote_w.saturating_sub(1))
                                    .collect::<String>()
                            )
                        } else {
                            format!("{:<remote_w$}", remote_str, remote_w = remote_w)
                        };
                        for ch in truncated_remote.chars() {
                            if cur_col < conn_inner.right() {
                                frame.buffer_mut()[(cur_col, row_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(Color::Rgb(220, 220, 220)));
                                cur_col += 1;
                            }
                        }
                        cur_col += 1;
                        // State
                        for ch in format!("{:<state_w$}", state_str, state_w = state_w).chars() {
                            if cur_col < conn_inner.right() {
                                frame.buffer_mut()[(cur_col, row_y)]
                                    .set_char(ch)
                                    .set_style(Style::default().fg(state_color));
                                cur_col += 1;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Returns true if either the physical disks box or mounted filesystems box overflows in standard 2x2 grid mode.
///
/// # Arguments
/// * `area` - Target bounding box for the tab.
/// * `disk_io` - Aggregate and per-disk I/O metrics.
/// * `disk_mounts` - List of mounted partition usage statistics.
pub fn is_disks_overflow(area: Rect, disk_io: &DiskIoInfo, disk_mounts: &[MountInfo]) -> bool {
    let top_right_h = (area.height / 2).saturating_sub(2) as usize;
    let top_right_w = ((area.width * 30) / 100).saturating_sub(2) as usize;
    let bot_right_h = (area.height.saturating_sub(area.height / 2)).saturating_sub(2) as usize;
    let bot_right_w = top_right_w;

    let physical_disks_lines = 6 + disk_io.disks.len() * 2;
    let mut physical_disks_overflow = physical_disks_lines > top_right_h || top_right_w < 10;
    if !physical_disks_overflow {
        let r_line = format!("Read:     {} ↑/s", format_bytes_dyn(disk_io.read_speed));
        let w_line = format!("Write:    {} ↓/s", format_bytes_dyn(disk_io.write_speed));
        let tr_line = format!(
            "Total R:  {}",
            format_bytes_dyn(disk_io.total_read_bytes as f64)
        );
        let tw_line = format!(
            "Total W:  {}",
            format_bytes_dyn(disk_io.total_write_bytes as f64)
        );
        if r_line.chars().count() > top_right_w
            || w_line.chars().count() > top_right_w
            || tr_line.chars().count() > top_right_w
            || tw_line.chars().count() > top_right_w
        {
            physical_disks_overflow = true;
        } else {
            for d in &disk_io.disks {
                let drive_line = format!("• {}: {}", d.name, d.model);
                let speed_line = format!(
                    "  {} ↑ / {} ↓",
                    format_bytes_dyn(d.read_speed),
                    format_bytes_dyn(d.write_speed)
                );
                if drive_line.chars().count() > top_right_w
                    || speed_line.chars().count() > top_right_w
                {
                    physical_disks_overflow = true;
                    break;
                }
            }
        }
    }

    let mut mount_rows_needed = 0;
    for m in disk_mounts {
        let header = format!("{} [{}] ({})", m.mount_point, m.device, m.fs_type);
        let h_lines = wrap_text(&header, bot_right_w.max(1));
        mount_rows_needed += h_lines.len() + 2;
    }
    let mounts_overflow = mount_rows_needed > bot_right_h || bot_right_w < 10;

    physical_disks_overflow || mounts_overflow
}

/// Renders the Disks & Storage (Tab 6) read/write throughput graphs, physical drives, package storage categories, and partition usage bars.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the tab.
/// * `disk_io` - Aggregate and per-disk I/O metrics.
/// * `disk_mounts` - List of mounted partition usage statistics.
/// * `disk_read_history` - Historical read speed samples for Braille graphing.
/// * `disk_write_history` - Historical write speed samples for Braille graphing.
/// * `storage_categories` - Detected package and runtime storage categories.
/// * `active_cat_idx` - Selected sub-tab category index.
/// * `scroll_offset` - Vertical scroll offset within the active category's item list.
/// * `box_tab` - Selected box index when in tabbed view (0: Graphs, 1: Physical Disks, 2: Storage, 3: Mounted Filesystems).
#[allow(clippy::too_many_arguments)]
pub fn render_disks_tab(
    frame: &mut Frame,
    area: Rect,
    disk_io: &DiskIoInfo,
    disk_mounts: &[MountInfo],
    disk_read_history: &[Option<f64>],
    disk_write_history: &[Option<f64>],
    storage_categories: &[PackageStorageCategory],
    active_cat_idx: usize,
    scroll_offset: usize,
    box_tab: usize,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let is_tabbed = is_disks_overflow(area, disk_io, disk_mounts);

    if is_tabbed {
        let sub_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);

        let titles = vec![
            "Graphs (a)",
            "Physical Disks (s)",
            "Store (d)",
            "Mounted Filesystems (f)",
        ];
        let tabs = Tabs::new(titles)
            .style(Style::default().fg(Color::Rgb(170, 170, 170)))
            .select(box_tab % 4)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                    .title(" View (a/s/d/f switch box) ".fg(Color::Rgb(170, 170, 170))),
            )
            .highlight_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold())
            .divider("|");

        frame.render_widget(tabs, sub_chunks[0]);

        let body = sub_chunks[1];
        match box_tab % 4 {
            0 => {
                let disk_read_label = format!("{} ↑/s", format_bytes_dyn(disk_io.read_speed));
                let disk_write_label = format!("{} ↓/s", format_bytes_dyn(disk_io.write_speed));
                if body.width >= 70 {
                    let graph_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(body);
                    render_gradient_chart(
                        frame,
                        graph_chunks[0],
                        "Disk Read (↑) History",
                        None,
                        Some(&disk_read_label),
                        Color::Rgb(60, 60, 60),
                        disk_read_history,
                    );
                    render_gradient_chart(
                        frame,
                        graph_chunks[1],
                        "Disk Write (↓) History",
                        None,
                        Some(&disk_write_label),
                        Color::Rgb(60, 60, 60),
                        disk_write_history,
                    );
                } else {
                    let graph_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(body);
                    render_gradient_chart(
                        frame,
                        graph_chunks[0],
                        "Disk Read (↑) History",
                        None,
                        Some(&disk_read_label),
                        Color::Rgb(60, 60, 60),
                        disk_read_history,
                    );
                    render_gradient_chart(
                        frame,
                        graph_chunks[1],
                        "Disk Write (↓) History",
                        None,
                        Some(&disk_write_label),
                        Color::Rgb(60, 60, 60),
                        disk_write_history,
                    );
                }
            }
            1 => {
                render_physical_disks_card(frame, body, disk_io);
            }
            2 => {
                render_package_storage_card(
                    frame,
                    body,
                    storage_categories,
                    active_cat_idx,
                    scroll_offset,
                );
            }
            _ => {
                render_mounted_filesystems_card(frame, body, disk_mounts);
            }
        }
    } else {
        let body_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let top_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(body_chunks[0]);

        let graph_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(top_chunks[0]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(body_chunks[1]);

        let disk_read_label = format!("{} ↑/s", format_bytes_dyn(disk_io.read_speed));
        let disk_write_label = format!("{} ↓/s", format_bytes_dyn(disk_io.write_speed));

        render_gradient_chart(
            frame,
            graph_chunks[0],
            "Disk Read (↑) History",
            None,
            Some(&disk_read_label),
            Color::Rgb(60, 60, 60),
            disk_read_history,
        );

        render_gradient_chart(
            frame,
            graph_chunks[1],
            "Disk Write (↓) History",
            None,
            Some(&disk_write_label),
            Color::Rgb(60, 60, 60),
            disk_write_history,
        );

        render_physical_disks_card(frame, top_chunks[1], disk_io);
        render_package_storage_card(
            frame,
            bottom_chunks[0],
            storage_categories,
            active_cat_idx,
            scroll_offset,
        );
        render_mounted_filesystems_card(frame, bottom_chunks[1], disk_mounts);
    }
}

/// Renders the Physical Disks block displaying read/write speeds, totals, and per-drive metrics.
fn render_physical_disks_card(frame: &mut Frame, area: Rect, disk_io: &DiskIoInfo) {
    let disks_block = Block::default()
        .title(" Physical Disks ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let disks_inner = disks_block.inner(area);
    frame.render_widget(disks_block, area);

    if disks_inner.height > 0 && disks_inner.width > 0 {
        let mut lines = vec![
            format!("Read:     {} ↑/s", format_bytes_dyn(disk_io.read_speed)),
            format!("Write:    {} ↓/s", format_bytes_dyn(disk_io.write_speed)),
            format!(
                "Total R:  {}",
                format_bytes_dyn(disk_io.total_read_bytes as f64)
            ),
            format!(
                "Total W:  {}",
                format_bytes_dyn(disk_io.total_write_bytes as f64)
            ),
            "".to_string(),
            "Drives:".to_string(),
        ];
        for d in &disk_io.disks {
            lines.push(format!("• {}: {}", d.name, d.model));
            lines.push(format!(
                "  {} ↑ / {} ↓",
                format_bytes_dyn(d.read_speed),
                format_bytes_dyn(d.write_speed)
            ));
        }
        for (idx, line_str) in lines.iter().enumerate() {
            let row = disks_inner.y + idx as u16;
            if row < disks_inner.bottom() {
                for (c_idx, ch) in line_str.chars().enumerate() {
                    let col = disks_inner.x + c_idx as u16;
                    if col < disks_inner.right() {
                        let color = if idx == 0 {
                            if disk_io.read_speed > 0.0 {
                                gradient_color(io_gradient_pct(disk_io.read_speed))
                            } else {
                                Color::Rgb(0, 85, 0)
                            }
                        } else if idx == 1 {
                            if disk_io.write_speed > 0.0 {
                                gradient_color(io_gradient_pct(disk_io.write_speed))
                            } else {
                                Color::Rgb(0, 85, 0)
                            }
                        } else if idx == 5 {
                            Color::Rgb(255, 255, 255)
                        } else if idx >= 7 && (idx - 7) % 2 == 0 {
                            let drive_idx = (idx - 7) / 2;
                            if let Some(d) = disk_io.disks.get(drive_idx) {
                                let max_speed = d.read_speed.max(d.write_speed);
                                if max_speed > 0.0 {
                                    gradient_color(io_gradient_pct(max_speed))
                                } else {
                                    Color::Rgb(0, 85, 0)
                                }
                            } else {
                                Color::Rgb(170, 170, 170)
                            }
                        } else if idx >= 6 && (idx - 6) % 2 == 0 {
                            Color::Rgb(220, 220, 220)
                        } else {
                            Color::Rgb(170, 170, 170)
                        };
                        frame.buffer_mut()[(col, row)]
                            .set_char(ch)
                            .set_style(Style::default().fg(color));
                    }
                }
            }
        }
    }
}

/// Renders the Package & Application Storage block displaying detected storage categories.
fn render_package_storage_card(
    frame: &mut Frame,
    area: Rect,
    storage_categories: &[PackageStorageCategory],
    active_cat_idx: usize,
    scroll_offset: usize,
) {
    let card_title = if storage_categories.is_empty() {
        " Storage ".to_string()
    } else {
        let cat_idx = active_cat_idx.min(storage_categories.len().saturating_sub(1));
        format!(" {} ", storage_categories[cat_idx].name)
    };
    let pkg_block = Block::default()
        .title(card_title.fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let pkg_inner = pkg_block.inner(area);
    frame.render_widget(pkg_block, area);

    if pkg_inner.height > 0 && pkg_inner.width > 0 {
        if storage_categories.is_empty() {
            let msg = "Scanning package and runtime storage in background...";
            let sub = "(Supported: Docker, Wine, Flatpak, Snap, Nix, APT, DNF, Pacman, npm • 20s refresh)";
            let row = pkg_inner.y + 1;
            for (c_idx, ch) in msg.chars().enumerate() {
                let col = pkg_inner.x + 2 + c_idx as u16;
                if col < pkg_inner.right() {
                    frame.buffer_mut()[(col, row)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(120, 120, 120)));
                }
            }
            let row2 = pkg_inner.y + 2;
            for (c_idx, ch) in sub.chars().enumerate() {
                let col = pkg_inner.x + 2 + c_idx as u16;
                if col < pkg_inner.right() {
                    frame.buffer_mut()[(col, row2)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(80, 80, 80)));
                }
            }
        } else {
            let cat_idx = active_cat_idx.min(storage_categories.len().saturating_sub(1));
            let active_cat = &storage_categories[cat_idx];
            let total_items = active_cat.items.len();

            // 1. Render Sub-Tabs Header Line (wrapping to 2 rows if overflowing)
            let mut subtab_rows = 1;
            let mut cur_row: u16 = 0;
            let mut cur_col = pkg_inner.x;
            for (i, cat) in storage_categories.iter().enumerate() {
                let is_active = i == cat_idx;
                let tab_label = if is_active {
                    format!("[ ▶ {} ({}) ] ", cat.name, cat.total_str)
                } else {
                    format!("[ {} ({}) ] ", cat.name, cat.total_str)
                };
                let tab_len = tab_label.chars().count() as u16;

                // Wrap to next row if overflowing first row
                if cur_row == 0 && cur_col > pkg_inner.x && (cur_col + tab_len) > pkg_inner.right()
                {
                    cur_row = 1;
                    cur_col = pkg_inner.x;
                    subtab_rows = 2;
                }

                let style = if is_active {
                    Style::default()
                        .fg(Color::Rgb(255, 255, 255))
                        .bold()
                        .bg(Color::Rgb(40, 40, 40))
                } else {
                    Style::default().fg(Color::Rgb(130, 130, 130))
                };

                let target_y = pkg_inner.y + cur_row;
                if target_y < pkg_inner.bottom() {
                    for ch in tab_label.chars() {
                        if cur_col < pkg_inner.right() {
                            frame.buffer_mut()[(cur_col, target_y)]
                                .set_char(ch)
                                .set_style(style);
                            cur_col += 1;
                        }
                    }
                }
            }

            // 2. Items list with scroll offset (clean 1-line text with size)
            let start_row_offset = subtab_rows + 1;
            let visible_rows =
                (pkg_inner.height as usize).saturating_sub(start_row_offset as usize);
            let max_scroll = total_items.saturating_sub(visible_rows);
            let start_idx = scroll_offset.min(max_scroll);

            for (row_offset, item) in
                (start_row_offset..).zip(active_cat.items.iter().skip(start_idx))
            {
                if row_offset >= pkg_inner.height {
                    break;
                }
                let row_y = pkg_inner.y + row_offset;
                let right_label = &item.size_str;
                let right_len = right_label.chars().count() as u16;

                // Right aligned size text
                let r_col = pkg_inner.right().saturating_sub(right_len + 1);
                for (c_idx, ch) in right_label.chars().enumerate() {
                    let col = r_col + c_idx as u16;
                    if col < pkg_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold());
                    }
                }

                // Left aligned name and detail
                let max_left_w = (r_col.saturating_sub(pkg_inner.x + 2)) as usize;
                let full_left = if item.detail.is_empty() {
                    format!("• {}", item.name)
                } else {
                    format!("• {} [{}]", item.name, item.detail)
                };

                let truncated_left: String = if full_left.chars().count() > max_left_w {
                    let take_len = max_left_w.saturating_sub(1);
                    let mut s: String = full_left.chars().take(take_len).collect();
                    s.push('…');
                    s
                } else {
                    full_left
                };

                for (c_idx, ch) in truncated_left.chars().enumerate() {
                    let col = pkg_inner.x + c_idx as u16;
                    if col < r_col {
                        let is_detail = ch == '[' || ch == ']' || (c_idx > item.name.len() + 2);
                        let color = if is_detail {
                            Color::Rgb(140, 140, 140)
                        } else {
                            Color::Rgb(230, 230, 230)
                        };
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(color));
                    }
                }
            }
        }
    }
}

/// Renders the Mounted Filesystems block displaying mounted partition progress bars and statistics.
fn render_mounted_filesystems_card(frame: &mut Frame, area: Rect, disk_mounts: &[MountInfo]) {
    let mounts_block = Block::default()
        .title(" Mounted Filesystems ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let mounts_inner = mounts_block.inner(area);
    frame.render_widget(mounts_block, area);

    if mounts_inner.height > 0 && mounts_inner.width > 0 {
        let mut row_offset = 0;
        let width = mounts_inner.width as usize;
        for m in disk_mounts {
            let header = format!("{} [{}] ({})", m.mount_point, m.device, m.fs_type);
            let header_lines = wrap_text(&header, width);

            if row_offset + header_lines.len() as u16 + 1 > mounts_inner.height {
                break;
            }
            let sub = format!(
                "{} / {} (Free: {})",
                format_bytes_dyn(m.used_bytes as f64),
                format_bytes_dyn(m.total_bytes as f64),
                format_bytes_dyn(m.free_bytes as f64)
            );

            for h_line in &header_lines {
                let row = mounts_inner.y + row_offset;
                for (c_idx, ch) in h_line.chars().enumerate() {
                    let col = mounts_inner.x + c_idx as u16;
                    if col < mounts_inner.right() {
                        frame.buffer_mut()[(col, row)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold());
                    }
                }
                row_offset += 1;
            }

            let row_bar = mounts_inner.y + row_offset;
            let total_bar = mounts_inner.width;
            let filled_len =
                ((total_bar as f64) * (m.used_pct.clamp(0.0, 100.0) / 100.0)).round() as u16;
            for c in 0..total_bar {
                let col = mounts_inner.x + c;
                if col >= mounts_inner.right() {
                    break;
                }
                let pct = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
                if c < filled_len {
                    let color = gradient_color(pct);
                    frame.buffer_mut()[(col, row_bar)]
                        .set_char('━')
                        .set_style(Style::default().fg(color));
                } else {
                    frame.buffer_mut()[(col, row_bar)]
                        .set_char('─')
                        .set_style(Style::default().fg(Color::Rgb(50, 50, 50)));
                }
            }
            row_offset += 1;

            if row_offset < mounts_inner.height {
                let row_sub = mounts_inner.y + row_offset;
                for (c_idx, ch) in sub.chars().enumerate() {
                    let col = mounts_inner.x + c_idx as u16;
                    if col < mounts_inner.right() {
                        frame.buffer_mut()[(col, row_sub)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(140, 140, 140)));
                    }
                }
                row_offset += 1;
            }
        }
    }
}

/// Draws a single-line horizontal gradient capacity bar with a left label and styled right text segments.
///
/// # Arguments
/// * `buf` - Direct Ratatui cell buffer.
/// * `area` - 1-row rectangular area.
/// * `label_left` - Left-aligned descriptor text.
/// * `pct` - Fill percentage (0.0 to 100.0).
/// * `segments` - Slice of right-aligned text segments `(text, color, is_bold)`.
fn draw_styled_bar(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    label_left: &str,
    pct: f64,
    segments: &[(&str, Color, bool)],
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut col = area.x;
    for ch in label_left.chars() {
        if col < area.right() {
            buf[(col, area.y)]
                .set_char(ch)
                .set_style(Style::default().fg(Color::Rgb(170, 170, 170)));
            col += 1;
        }
    }
    col = (col + 1).min(area.right());

    let r_len: usize = segments.iter().map(|(s, _, _)| s.chars().count()).sum();
    let bar_end = area.right().saturating_sub(r_len as u16 + 1);

    if bar_end > col {
        let total_bar = bar_end - col;
        let pct_clamped = pct.clamp(0.0, 100.0);
        let filled_len = ((total_bar as f64) * (pct_clamped / 100.0)).round() as u16;

        for c in 0..total_bar {
            let bar_col = col + c;
            let bar_pct = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
            if c < filled_len {
                let color = gradient_color(bar_pct);
                buf[(bar_col, area.y)]
                    .set_char('━')
                    .set_style(Style::default().fg(color));
            } else {
                buf[(bar_col, area.y)]
                    .set_char('─')
                    .set_style(Style::default().fg(Color::Rgb(50, 50, 50)));
            }
        }
    }

    let mut r_col = area.right().saturating_sub(r_len as u16);
    for &(seg_str, seg_color, seg_bold) in segments {
        let mut style = Style::default().fg(seg_color);
        if seg_bold {
            style = style.bold();
        }
        for ch in seg_str.chars() {
            if r_col < area.right() {
                buf[(r_col, area.y)].set_char(ch).set_style(style);
                r_col += 1;
            }
        }
    }
}

/// Convenience wrapper around `draw_styled_bar` for simple single right-aligned labels.
///
/// # Arguments
/// * `buf` - Direct Ratatui cell buffer.
/// * `area` - 1-row rectangular area.
/// * `label_left` - Left-aligned descriptor text.
/// * `label_right` - Right-aligned value text.
/// * `pct` - Fill percentage (0.0 to 100.0).
fn draw_labeled_bar(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    label_left: &str,
    label_right: &str,
    pct: f64,
) {
    draw_styled_bar(
        buf,
        area,
        label_left,
        pct,
        &[(label_right, Color::Rgb(220, 220, 220), false)],
    );
}

/// Formats the complete System Overview telemetry block into a plain multiline text string for clipboard copying.
///
/// # Arguments
/// * `sys_info` - System general metadata.
/// * `cpu_model` - Marketing CPU model identifier.
/// * `gpu` - GPU metrics and device name.
/// * `ram_info` - Memory capacity and frequency string.
///
/// # Returns
/// Newline-delimited text ready to copy.
pub fn format_system_overview_copy_text(
    sys_info: &SystemGeneralInfo,
    cpu_model: &str,
    gpu: &GpuMetrics,
    ram_info: &str,
) -> String {
    let vram_gb = ((gpu.vram_total_mb as f64) / 1024.0).round() as u64;
    let gpu_label = if vram_gb > 0 {
        format!("{} ({}GB)", gpu.name, vram_gb)
    } else {
        gpu.name.clone()
    };

    let mut lines = Vec::new();
    lines.push(format!("Host:     {}", sys_info.hostname));
    lines.push(format!("OS:       {}", sys_info.os_name));
    lines.push(format!("Kernel:   {}", sys_info.kernel));
    lines.push(format!("Uptime:   {}", format_uptime(sys_info.uptime_secs)));
    lines.push(format!("Desktop:  {}", sys_info.desktop));
    lines.push(format!("Shell:    {}", sys_info.shell));
    lines.push(format!("CPU:      {}", cpu_model));
    lines.push(format!("GPU:      {}", gpu_label));
    lines.push(format!("RAM:      {}", ram_info));
    let net_details = match (
        !sys_info.net_interface.is_empty(),
        sys_info.net_speed_mbps,
        sys_info.net_duplex.as_str(),
    ) {
        (true, s, d) if s > 0 && d != "Unknown" && !d.is_empty() => {
            format!(
                "{} ({}, {} Mbps, {} Duplex)",
                sys_info.local_ip, sys_info.net_interface, s, d
            )
        }
        (true, s, _) if s > 0 => {
            format!(
                "{} ({}, {} Mbps)",
                sys_info.local_ip, sys_info.net_interface, s
            )
        }
        (true, _, d) if d != "Unknown" && !d.is_empty() => {
            format!(
                "{} ({}, {} Duplex)",
                sys_info.local_ip, sys_info.net_interface, d
            )
        }
        (true, _, _) => {
            format!("{} ({})", sys_info.local_ip, sys_info.net_interface)
        }
        (false, s, d) if s > 0 && d != "Unknown" && !d.is_empty() => {
            format!("{} ({} Mbps, {} Duplex)", sys_info.local_ip, s, d)
        }
        (false, s, _) if s > 0 => format!("{} ({} Mbps)", sys_info.local_ip, s),
        (false, _, d) if d != "Unknown" && !d.is_empty() => {
            format!("{} ({} Duplex)", sys_info.local_ip, d)
        }
        _ => sys_info.local_ip.clone(),
    };
    lines.push(format!("Local IP: {}", net_details));
    lines.push(format!("Locale:   {}", sys_info.locale));

    if sys_info.displays.is_empty() {
        lines.push("Display:  Headless / Default".to_string());
    } else {
        for d in &sys_info.displays {
            let diag_str = d
                .diagonal_inch
                .map(|n| format!(" in {}\"", n))
                .unwrap_or_default();
            let hz_str = d
                .refresh_rate_hz
                .map(|h| format!(", {} Hz", h))
                .unwrap_or_default();
            let ext_str = if d.is_external {
                " [External]"
            } else {
                " [Internal]"
            };
            let mut clean_name = d.name.trim();
            while clean_name.starts_with("Display") || clean_name.starts_with("display") {
                clean_name = clean_name[7..].trim();
                clean_name = clean_name.trim_start_matches(':').trim();
            }
            if clean_name.is_empty() {
                lines.push(format!(
                    "Display:  {}{}{}{}",
                    d.resolution, diag_str, hz_str, ext_str
                ));
            } else {
                lines.push(format!(
                    "Display:  ({}) {}{}{}{}",
                    clean_name, d.resolution, diag_str, hz_str, ext_str
                ));
            }
        }
    }

    lines.join("\n")
}

/// Renders the System Overview telemetry metadata card.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the overview card.
/// * `sys_info` - Host system overview metadata.
/// * `cpu_model` - Marketing CPU model identifier string.
/// * `num_cores` - Count of logical CPU cores.
/// * `ram_info` - DMI memory capacity, speed, and DDR generation string.
/// * `gpu` - GPU metrics.
/// * `copied` - Whether the overview copy button feedback is currently active.
#[allow(clippy::too_many_arguments)]
fn render_general_overview_card(
    frame: &mut Frame,
    area: Rect,
    sys_info: &SystemGeneralInfo,
    cpu_model: &str,
    num_cores: usize,
    ram_info: &str,
    gpu: &GpuMetrics,
    copied: bool,
) {
    let sys_block = Block::default()
        .title(" System Overview ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let sys_inner = sys_block.inner(area);
    frame.render_widget(sys_block, area);

    if sys_inner.height > 0 && sys_inner.width > 0 {
        let vram_gb = ((gpu.vram_total_mb as f64) / 1024.0).round() as u64;
        let gpu_label = if vram_gb > 0 {
            format!("{} ({}GB)", gpu.name, vram_gb)
        } else {
            gpu.name.clone()
        };

        let mut lines = Vec::new();
        lines.push(format!("Host:     {}", sys_info.hostname));
        lines.push(format!("OS:       {}", sys_info.os_name));
        lines.push(format!("Kernel:   {}", sys_info.kernel));
        lines.push(format!("Uptime:   {}", format_uptime(sys_info.uptime_secs)));
        lines.push(format!("Desktop:  {}", sys_info.desktop));
        lines.push(format!("Shell:    {}", sys_info.shell));
        lines.push(format!("CPU:      {}", cpu_model));
        lines.push(format!("GPU:      {}", gpu_label));
        lines.push(format!("RAM:      {}", ram_info));
        let net_details = match (
            !sys_info.net_interface.is_empty(),
            sys_info.net_speed_mbps,
            sys_info.net_duplex.as_str(),
        ) {
            (true, s, d) if s > 0 && d != "Unknown" && !d.is_empty() => {
                format!(
                    "{} ({}, {} Mbps, {} Duplex)",
                    sys_info.local_ip, sys_info.net_interface, s, d
                )
            }
            (true, s, _) if s > 0 => {
                format!(
                    "{} ({}, {} Mbps)",
                    sys_info.local_ip, sys_info.net_interface, s
                )
            }
            (true, _, d) if d != "Unknown" && !d.is_empty() => {
                format!(
                    "{} ({}, {} Duplex)",
                    sys_info.local_ip, sys_info.net_interface, d
                )
            }
            (true, _, _) => {
                format!("{} ({})", sys_info.local_ip, sys_info.net_interface)
            }
            (false, s, d) if s > 0 && d != "Unknown" && !d.is_empty() => {
                format!("{} ({} Mbps, {} Duplex)", sys_info.local_ip, s, d)
            }
            (false, s, _) if s > 0 => format!("{} ({} Mbps)", sys_info.local_ip, s),
            (false, _, d) if d != "Unknown" && !d.is_empty() => {
                format!("{} ({} Duplex)", sys_info.local_ip, d)
            }
            _ => sys_info.local_ip.clone(),
        };
        lines.push(format!("Local IP: {}", net_details));
        lines.push(format!("Locale:   {}", sys_info.locale));

        if sys_info.displays.is_empty() {
            lines.push("Display:  Headless / Default".to_string());
        } else {
            for d in &sys_info.displays {
                let diag_str = d
                    .diagonal_inch
                    .map(|n| format!(" in {}\"", n))
                    .unwrap_or_default();
                let hz_str = d
                    .refresh_rate_hz
                    .map(|h| format!(", {} Hz", h))
                    .unwrap_or_default();
                let ext_str = if d.is_external {
                    " [External]"
                } else {
                    " [Internal]"
                };
                let mut clean_name = d.name.trim();
                while clean_name.starts_with("Display") || clean_name.starts_with("display") {
                    clean_name = clean_name[7..].trim();
                    clean_name = clean_name.trim_start_matches(':').trim();
                }
                if clean_name.is_empty() {
                    lines.push(format!(
                        "Display:  {}{}{}{}",
                        d.resolution, diag_str, hz_str, ext_str
                    ));
                } else {
                    lines.push(format!(
                        "Display:  ({}) {}{}{}{}",
                        clean_name, d.resolution, diag_str, hz_str, ext_str
                    ));
                }
            }
        }

        let is_kernel_outdated = !is_lts_or_latest_kernel(&sys_info.kernel);
        let is_ram_low = is_ram_under_8gb(ram_info);
        let is_cpu_low = num_cores < 4;

        for (idx, line) in lines.iter().enumerate() {
            let row = sys_inner.y + idx as u16;
            if row < sys_inner.bottom() {
                let label_end = line.find(':').map(|p| p + 1).unwrap_or(0);
                let is_kernel_line = line.starts_with("Kernel:");
                let is_ram_line = line.starts_with("RAM:");
                let is_cpu_line = line.starts_with("CPU:");
                for (c_idx, ch) in line.chars().enumerate() {
                    let col = sys_inner.x + c_idx as u16;
                    if col < sys_inner.right() {
                        let color = if c_idx < label_end {
                            Color::Rgb(220, 220, 220)
                        } else if (is_kernel_line && is_kernel_outdated)
                            || (is_ram_line && is_ram_low)
                            || (is_cpu_line && is_cpu_low)
                        {
                            Color::Rgb(255, 220, 0)
                        } else {
                            Color::Rgb(180, 180, 180)
                        };
                        frame.buffer_mut()[(col, row)]
                            .set_char(ch)
                            .set_style(Style::default().fg(color));
                    }
                }
            }
        }

        // Bottom right Copy button
        let btn_text = if copied {
            "[ ✓ Copied! ]"
        } else {
            "[ 'c' Copy ]"
        };
        let btn_len = btn_text.chars().count() as u16;
        let btn_x = sys_inner.right().saturating_sub(btn_len);
        let btn_y = sys_inner.bottom().saturating_sub(1);

        if btn_x < sys_inner.right() && btn_y < sys_inner.bottom() {
            let style = if copied {
                Style::default().fg(Color::Rgb(0, 255, 128))
            } else {
                Style::default().fg(Color::Rgb(100, 200, 255))
            };
            for (idx, ch) in btn_text.chars().enumerate() {
                let col = btn_x + idx as u16;
                if col < sys_inner.right() {
                    frame.buffer_mut()[(col, btn_y)]
                        .set_char(ch)
                        .set_style(style);
                }
            }
        }
    }
}

/// Renders the Battery & Power telemetry metadata card.
fn render_general_battery_card(frame: &mut Frame, area: Rect, battery: Option<&BatteryInfo>) {
    let pwr_block = Block::default()
        .title(" Battery & Power ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let pwr_inner = pwr_block.inner(area);
    frame.render_widget(pwr_block, area);

    if pwr_inner.height > 0 && pwr_inner.width > 0 {
        if let Some(bat) = battery {
            let cap_str = format!("{}%", bat.capacity_pct);
            let state_str = match (bat.power_w, bat.status.to_lowercase().as_str()) {
                (Some(w), "charging") => format!("Charging (+{:.1}W)", w),
                (Some(w), "discharging") => format!("Discharging (-{:.1}W)", w),
                (Some(w), _) if w > 0.0 => format!("{} ({:.1}W)", bat.status, w),
                _ => bat.status.clone(),
            };

            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: pwr_inner.x,
                    y: pwr_inner.y,
                    width: pwr_inner.width,
                    height: 1,
                },
                &format!("Battery ({}):", bat.name),
                &format!("{} [{}]", cap_str, state_str),
                bat.capacity_pct as f64,
            );

            let mut pwr_lines = Vec::new();
            pwr_lines.push(format!("Status:   {}", bat.status));
            if let Some(w) = bat.power_w {
                if bat.status.to_lowercase() == "charging" {
                    pwr_lines.push(format!("Rate:     +{:.1} W (Charging)", w));
                } else if bat.status.to_lowercase() == "discharging" {
                    pwr_lines.push(format!("Rate:     -{:.1} W (Drainage)", w));
                } else {
                    pwr_lines.push(format!("Rate:     {:.1} W", w));
                }
            }
            pwr_lines.push(format!("Health:   {}", bat.health));
            pwr_lines.push(format!(
                "Energy:   {:.1} / {:.1} Wh",
                bat.energy_now_wh.unwrap_or(0.0),
                bat.energy_full_wh.unwrap_or(0.0)
            ));

            for (idx, line) in pwr_lines.iter().enumerate() {
                let row = pwr_inner.y + 2 + idx as u16;
                if row < pwr_inner.bottom() {
                    let label_end = line.find(':').map(|p| p + 1).unwrap_or(0);
                    for (c_idx, ch) in line.chars().enumerate() {
                        let col = pwr_inner.x + c_idx as u16;
                        if col < pwr_inner.right() {
                            let color = if c_idx < label_end {
                                Color::Rgb(220, 220, 220)
                            } else {
                                Color::Rgb(180, 180, 180)
                            };
                            frame.buffer_mut()[(col, row)]
                                .set_char(ch)
                                .set_style(Style::default().fg(color));
                        }
                    }
                }
            }
        } else {
            let pwr_line = "Power Source: AC Connected ";
            for (c_idx, ch) in pwr_line.chars().enumerate() {
                let col = pwr_inner.x + c_idx as u16;
                if col < pwr_inner.right() {
                    frame.buffer_mut()[(col, pwr_inner.y)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(0, 255, 128)));
                }
            }
            let pwr_sub_lines = ["Hardware sensors indicate direct wall power."];
            for (idx, line) in pwr_sub_lines.iter().enumerate() {
                let row = pwr_inner.y + 2 + idx as u16;
                if row < pwr_inner.bottom() {
                    for (c_idx, ch) in line.chars().enumerate() {
                        let col = pwr_inner.x + c_idx as u16;
                        if col < pwr_inner.right() {
                            frame.buffer_mut()[(col, row)]
                                .set_char(ch)
                                .set_style(Style::default().fg(Color::Rgb(140, 140, 140)));
                        }
                    }
                }
            }
        }
    }
}

/// Renders the CPU performance card with total bar and per-core grid.
#[allow(clippy::too_many_arguments)]
fn render_general_cpu_card(
    frame: &mut Frame,
    area: Rect,
    global_cpu_usage: f64,
    core_usages: &[f64],
    cpu_cur_mhz: f64,
    cpu_min_mhz: f64,
    cpu_max_mhz: f64,
    cpu_temp: u32,
    cpu_model: &str,
) {
    let mut cpu_block = Block::default()
        .title(" CPU ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    if !cpu_model.is_empty() {
        cpu_block = cpu_block.title(
            Line::from(format!(" {} ", cpu_model).fg(Color::Rgb(170, 170, 170)))
                .alignment(ratatui::layout::Alignment::Right),
        );
    }
    let cpu_inner = cpu_block.inner(area);
    frame.render_widget(cpu_block, area);

    if cpu_inner.height > 0 && cpu_inner.width > 0 {
        let cpu_freq_pct = if cpu_max_mhz > cpu_min_mhz {
            ((cpu_cur_mhz - cpu_min_mhz) / (cpu_max_mhz - cpu_min_mhz) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let cpu_freq_color = gradient_color(cpu_freq_pct);
        let cpu_freq_str = format_freq(cpu_cur_mhz);

        let cpu_temp_pct = ((cpu_temp as f64 - 25.0) / 75.0 * 100.0).clamp(0.0, 100.0);
        let cpu_temp_color = gradient_color(cpu_temp_pct);
        let cpu_temp_str = format!("{}°C", cpu_temp);

        let cpu_pct_str = format!("{:.0}% [", global_cpu_usage);
        let cpu_end_str = "]".to_string();

        draw_styled_bar(
            frame.buffer_mut(),
            Rect {
                x: cpu_inner.x,
                y: cpu_inner.y,
                width: cpu_inner.width,
                height: 1,
            },
            "CPU Total:",
            global_cpu_usage,
            &[
                (&cpu_pct_str, Color::Rgb(220, 220, 220), false),
                (&cpu_freq_str, cpu_freq_color, false),
                (", ", Color::Rgb(170, 170, 170), false),
                (&cpu_temp_str, cpu_temp_color, false),
                (&cpu_end_str, Color::Rgb(220, 220, 220), false),
            ],
        );

        let start_row = 1u16;
        let avail_rows = (cpu_inner.bottom().saturating_sub(cpu_inner.y + start_row)) as usize;
        let num_cores = core_usages.len();
        let (cols, show_bar) = if num_cores.div_ceil(2) <= avail_rows || cpu_inner.width < 30 {
            (2, true)
        } else if num_cores.div_ceil(3) <= avail_rows || cpu_inner.width < 45 {
            (3, true)
        } else {
            (4, false)
        };
        let rows_per_col = num_cores.div_ceil(cols);
        let spacing = 2u16;
        let total_gap = spacing * (cols as u16 - 1);
        let col_w = cpu_inner.width.saturating_sub(total_gap) / cols as u16;

        if col_w > 0 {
            for r in 0..rows_per_col {
                let row_y = cpu_inner.y + start_row + r as u16;
                if row_y >= cpu_inner.bottom() {
                    break;
                }

                for c in 0..cols {
                    let core_idx = c * rows_per_col + r;
                    if let Some(&usage) = core_usages.get(core_idx) {
                        let col_x = cpu_inner.x + (c as u16) * (col_w + spacing);
                        let w = if c == cols - 1 {
                            cpu_inner.right().saturating_sub(col_x)
                        } else {
                            col_w
                        };

                        if show_bar {
                            draw_labeled_bar(
                                frame.buffer_mut(),
                                Rect {
                                    x: col_x,
                                    y: row_y,
                                    width: w,
                                    height: 1,
                                },
                                &format!("C{:<2}:", core_idx),
                                &format!("{:.0}%", usage),
                                usage,
                            );
                        } else {
                            let label = format!("C{:<2}: ", core_idx);
                            let val_str = format!("{:>3.0}%", usage);
                            let color = gradient_color(usage.clamp(0.0, 100.0));

                            let mut cx = col_x;
                            for ch in label.chars() {
                                if cx < col_x + w && cx < cpu_inner.right() {
                                    frame.buffer_mut()[(cx, row_y)]
                                        .set_char(ch)
                                        .set_style(Style::default().fg(Color::Rgb(170, 170, 170)));
                                    cx += 1;
                                }
                            }
                            for ch in val_str.chars() {
                                if cx < col_x + w && cx < cpu_inner.right() {
                                    frame.buffer_mut()[(cx, row_y)]
                                        .set_char(ch)
                                        .set_style(Style::default().fg(color));
                                    cx += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Renders the Memory and Swap utilization card.
fn render_general_memory_card(frame: &mut Frame, area: Rect, mem: &MemoryMetrics, ram_info: &str) {
    let mut mem_block = Block::default()
        .title(" Memory ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    if !ram_info.is_empty() {
        mem_block = mem_block.title(
            Line::from(format!(" {} ", ram_info).fg(Color::Rgb(170, 170, 170)))
                .alignment(ratatui::layout::Alignment::Right),
        );
    }
    let mem_inner = mem_block.inner(area);
    frame.render_widget(mem_block, area);

    if mem_inner.height > 0 && mem_inner.width > 0 {
        let ram_pct = if mem.total_mem_mb > 0 {
            (mem.used_mem_mb as f64 / mem.total_mem_mb as f64) * 100.0
        } else {
            0.0
        };
        let ram_used_gb = mem.used_mem_mb as f64 / 1024.0;
        let ram_total_gb = mem.total_mem_mb as f64 / 1024.0;

        draw_labeled_bar(
            frame.buffer_mut(),
            Rect {
                x: mem_inner.x,
                y: mem_inner.y,
                width: mem_inner.width,
                height: 1,
            },
            "RAM:",
            &format!(
                "{:.1}% ({:.1}/{:.1} GB)",
                ram_pct, ram_used_gb, ram_total_gb
            ),
            ram_pct,
        );

        if mem_inner.height > 1 {
            let swap_pct = if mem.total_swap_mb > 0 {
                (mem.used_swap_mb as f64 / mem.total_swap_mb as f64) * 100.0
            } else {
                0.0
            };
            let swap_used_gb = mem.used_swap_mb as f64 / 1024.0;
            let swap_total_gb = mem.total_swap_mb as f64 / 1024.0;

            let row_y = if mem_inner.height > 2 {
                mem_inner.y + 2
            } else {
                mem_inner.y + 1
            };

            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: mem_inner.x,
                    y: row_y,
                    width: mem_inner.width,
                    height: 1,
                },
                "Swap:",
                &format!(
                    "{:.1}% ({:.1}/{:.1} GB)",
                    swap_pct, swap_used_gb, swap_total_gb
                ),
                swap_pct,
            );
        }
    }
}

/// Renders the high-resource-consuming processes card.
fn render_general_processes_card(
    frame: &mut Frame,
    area: Rect,
    processes: &[ProcessInfo],
    mem: &MemoryMetrics,
    gpu: &GpuMetrics,
    num_cores: usize,
) {
    let heavy_block = Block::default()
        .title(" High Resource Processes ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let heavy_inner = heavy_block.inner(area);
    frame.render_widget(heavy_block, area);

    if heavy_inner.height > 0 && heavy_inner.width > 0 {
        let empty_set = HashSet::new();
        let grouped_procs = group_processes_for_simple_view(
            processes,
            &empty_set,
            ProcessSortColumn::Cpu,
            false,
            "",
        );
        let mut heavy_procs: Vec<(&ProcessInfo, f64, f64, f64)> = grouped_procs
            .iter()
            .filter_map(|p| {
                let all_core_cpu_pct = if num_cores > 0 {
                    p.cpu_percent / (num_cores as f64)
                } else {
                    p.cpu_percent
                };
                let mem_pct = if mem.total_mem_mb > 0 {
                    (p.rss_kb as f64 / (mem.total_mem_mb as f64 * 1024.0)) * 100.0
                } else {
                    0.0
                };
                let gpu_vram_pct = if gpu.vram_total_mb > 0 {
                    (p.gpu_mem_kb as f64 / (gpu.vram_total_mb as f64 * 1024.0)) * 100.0
                } else {
                    0.0
                };

                let is_heavy_cpu = all_core_cpu_pct >= 50.0;
                let is_heavy_mem = mem_pct >= 30.0;
                let is_heavy_gpu = p.gpu_percent >= 30.0;
                let is_heavy_vram = gpu_vram_pct >= 30.0;

                if is_heavy_cpu || is_heavy_mem || is_heavy_gpu || is_heavy_vram {
                    let max_pct = all_core_cpu_pct
                        .max(mem_pct)
                        .max(p.gpu_percent)
                        .max(gpu_vram_pct);
                    Some((p, mem_pct, gpu_vram_pct, max_pct))
                } else {
                    None
                }
            })
            .collect();

        heavy_procs.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));

        if heavy_procs.is_empty() {
            let msg = "No heavy processes found.";
            let msg_len = msg.chars().count() as u16;
            let msg_x = heavy_inner.x + (heavy_inner.width.saturating_sub(msg_len)) / 2;
            let msg_y = heavy_inner.y + heavy_inner.height / 2;
            if msg_y < heavy_inner.bottom() {
                for (c_idx, ch) in msg.chars().enumerate() {
                    let col = msg_x + c_idx as u16;
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, msg_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(120, 120, 120)));
                    }
                }
            }
        } else {
            let hdr_pid = "PID";
            let hdr_name = "NAME";
            let hdr_cpu = "CPU%";
            let hdr_mem = "MEM%";
            let hdr_gpu = "GPU%";
            let hdr_vram = "VRAM%";

            let name_w = (heavy_inner.width.saturating_sub(7 + 8 + 8 + 8 + 8 + 2) as usize).max(8);
            let header_str = format!(
                "{:<7}{:<name_w$} {:>7} {:>7} {:>7} {:>7}",
                hdr_pid,
                hdr_name,
                hdr_cpu,
                hdr_mem,
                hdr_gpu,
                hdr_vram,
                name_w = name_w
            );

            for (c_idx, ch) in header_str.chars().enumerate() {
                let col = heavy_inner.x + c_idx as u16;
                if col < heavy_inner.right() {
                    frame.buffer_mut()[(col, heavy_inner.y)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(200, 200, 200)));
                }
            }

            for (idx, &(p, mem_pct, gpu_vram_pct, _)) in heavy_procs.iter().enumerate() {
                let row_y = heavy_inner.y + 1 + idx as u16;
                if row_y >= heavy_inner.bottom() {
                    break;
                }

                let mut name_display = p.name.clone();
                if name_display.chars().count() > name_w {
                    name_display = format!(
                        "{}…",
                        p.name
                            .chars()
                            .take(name_w.saturating_sub(1))
                            .collect::<String>()
                    );
                }

                let pid_str = format!("{:<7}", p.pid);
                let cpu_val_str = format!("{:>6.1}% ", p.cpu_percent);
                let mem_val_str = format!("{:>6.1}% ", mem_pct);
                let gpu_val_str = format!("{:>6.1}% ", p.gpu_percent);
                let vram_val_str = format!("{:>6.1}%", gpu_vram_pct);

                let mut col = heavy_inner.x;
                // PID
                for ch in pid_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(160, 160, 160)));
                        col += 1;
                    }
                }
                // NAME
                let name_tokens: Vec<&str> = name_display.split_whitespace().collect();
                let mut name_cur_x = col;
                let mut in_args = false;
                for (t_idx, &tok) in name_tokens.iter().enumerate() {
                    if t_idx > 0 && name_cur_x < heavy_inner.right() {
                        frame.buffer_mut()[(name_cur_x, row_y)].set_char(' ');
                        name_cur_x += 1;
                    }
                    if tok.starts_with('-') {
                        in_args = true;
                    }
                    let tok_style = if in_args {
                        Style::default().fg(Color::Rgb(110, 110, 110))
                    } else {
                        Style::default().fg(Color::Rgb(240, 240, 240))
                    };
                    for ch in tok.chars() {
                        if name_cur_x < heavy_inner.right() && name_cur_x < col + name_w as u16 {
                            frame.buffer_mut()[(name_cur_x, row_y)]
                                .set_char(ch)
                                .set_style(tok_style);
                            name_cur_x += 1;
                        }
                    }
                }
                col += name_w as u16 + 1;
                // CPU%
                let all_core_cpu_pct = if num_cores > 0 {
                    p.cpu_percent / (num_cores as f64)
                } else {
                    p.cpu_percent
                };
                let cpu_color = if all_core_cpu_pct >= 50.0 {
                    gradient_color(all_core_cpu_pct.min(100.0))
                } else {
                    Color::Rgb(140, 140, 140)
                };
                for ch in cpu_val_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(cpu_color));
                        col += 1;
                    }
                }
                // MEM%
                let mem_color = if mem_pct >= 30.0 {
                    gradient_color(mem_pct.min(100.0))
                } else {
                    Color::Rgb(140, 140, 140)
                };
                for ch in mem_val_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(mem_color));
                        col += 1;
                    }
                }
                // GPU%
                let gpu_color = if p.gpu_percent >= 30.0 {
                    gradient_color(p.gpu_percent.min(100.0))
                } else {
                    Color::Rgb(140, 140, 140)
                };
                for ch in gpu_val_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(gpu_color));
                        col += 1;
                    }
                }
                // VRAM%
                let vram_color = if gpu_vram_pct >= 30.0 {
                    gradient_color(gpu_vram_pct.min(100.0))
                } else {
                    Color::Rgb(140, 140, 140)
                };
                for ch in vram_val_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(vram_color));
                        col += 1;
                    }
                }
            }
        }
    }
}

/// Renders the GPU utilization and VRAM allocation card.
fn render_general_gpu_card(frame: &mut Frame, area: Rect, gpu: &GpuMetrics) {
    let mut gpu_block = Block::default()
        .title(" GPU ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    if !gpu.name.is_empty() && gpu.name != "None" {
        gpu_block = gpu_block.title(
            Line::from(format!(" {} ", gpu.name).fg(Color::Rgb(170, 170, 170)))
                .alignment(ratatui::layout::Alignment::Right),
        );
    }
    let gpu_inner = gpu_block.inner(area);
    frame.render_widget(gpu_block, area);

    if gpu_inner.height > 0 && gpu_inner.width > 0 {
        let gpu_freq_pct = if gpu.max_mhz > gpu.min_mhz {
            ((gpu.cur_mhz - gpu.min_mhz) / (gpu.max_mhz - gpu.min_mhz) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let gpu_freq_color = gradient_color(gpu_freq_pct);
        let gpu_freq_str = format_freq(gpu.cur_mhz);

        let gpu_temp_pct = ((gpu.temp_c as f64 - 25.0) / 75.0 * 100.0).clamp(0.0, 100.0);
        let gpu_temp_color = gradient_color(gpu_temp_pct);
        let gpu_temp_str = format!("{}°C", gpu.temp_c);

        let gpu_pct_str = format!("{:.1}% [", gpu.utilization_pct);
        let gpu_end_str = "]".to_string();

        draw_styled_bar(
            frame.buffer_mut(),
            Rect {
                x: gpu_inner.x,
                y: gpu_inner.y,
                width: gpu_inner.width,
                height: 1,
            },
            "GPU Core:",
            gpu.utilization_pct,
            &[
                (&gpu_pct_str, Color::Rgb(220, 220, 220), false),
                (&gpu_freq_str, gpu_freq_color, false),
                (", ", Color::Rgb(170, 170, 170), false),
                (&gpu_temp_str, gpu_temp_color, false),
                (&gpu_end_str, Color::Rgb(220, 220, 220), false),
            ],
        );

        if gpu_inner.height > 1 {
            let vram_pct = if gpu.vram_total_mb > 0 {
                (gpu.vram_used_mb as f64 / gpu.vram_total_mb as f64) * 100.0
            } else {
                0.0
            };
            let row_y = if gpu_inner.height > 2 {
                gpu_inner.y + 2
            } else {
                gpu_inner.y + 1
            };
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: gpu_inner.x,
                    y: row_y,
                    width: gpu_inner.width,
                    height: 1,
                },
                "VRAM:",
                &format!(
                    "{:.1}% ({}/{} MB)",
                    vram_pct, gpu.vram_used_mb, gpu.vram_total_mb
                ),
                vram_pct,
            );
        }
    }
}

/// Renders the Network throughput card with RX and TX bandwidth.
fn render_general_net_card(frame: &mut Frame, area: Rect, net_ifaces: &[NetInterfaceInfo]) {
    let net_block = Block::default()
        .title(" Network ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let net_inner = net_block.inner(area);
    frame.render_widget(net_block, area);

    if net_inner.height > 0 && net_inner.width > 0 {
        let primary_iface = net_ifaces
            .iter()
            .find(|i| i.operstate == "up")
            .or_else(|| net_ifaces.first());
        let rx_speed = primary_iface.map(|i| i.rx_speed).unwrap_or(0.0);
        let tx_speed = primary_iface.map(|i| i.tx_speed).unwrap_or(0.0);

        draw_labeled_bar(
            frame.buffer_mut(),
            Rect {
                x: net_inner.x,
                y: net_inner.y,
                width: net_inner.width,
                height: 1,
            },
            "Download (RX):",
            &format!("{} ↓/s", format_bytes_dyn(rx_speed)),
            io_gradient_pct(rx_speed),
        );

        if net_inner.height > 1 {
            let row_y = if net_inner.height > 2 {
                net_inner.y + 2
            } else {
                net_inner.y + 1
            };
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: net_inner.x,
                    y: row_y,
                    width: net_inner.width,
                    height: 1,
                },
                "Upload (TX):",
                &format!("{} ↑/s", format_bytes_dyn(tx_speed)),
                io_gradient_pct(tx_speed),
            );
        }
    }
}

/// Renders the Disk IO throughput card with read and write rates.
fn render_general_disk_card(frame: &mut Frame, area: Rect, disk_io: &DiskIoInfo) {
    let disk_block = Block::default()
        .title(" IO ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let disk_inner = disk_block.inner(area);
    frame.render_widget(disk_block, area);

    if disk_inner.height > 0 && disk_inner.width > 0 {
        draw_labeled_bar(
            frame.buffer_mut(),
            Rect {
                x: disk_inner.x,
                y: disk_inner.y,
                width: disk_inner.width,
                height: 1,
            },
            "Disk Read (↑):",
            &format!("{} ↑/s", format_bytes_dyn(disk_io.read_speed)),
            io_gradient_pct(disk_io.read_speed),
        );

        if disk_inner.height > 1 {
            let row_y = if disk_inner.height > 2 {
                disk_inner.y + 2
            } else {
                disk_inner.y + 1
            };
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: disk_inner.x,
                    y: row_y,
                    width: disk_inner.width,
                    height: 1,
                },
                "Disk Write (↓):",
                &format!("{} ↓/s", format_bytes_dyn(disk_io.write_speed)),
                io_gradient_pct(disk_io.write_speed),
            );
        }
    }
}

/// Renders the mounted filesystem storage partitions card.
fn render_general_storage_card(frame: &mut Frame, area: Rect, disk_mounts: &[MountInfo]) {
    let storage_block = Block::default()
        .title(" Partitions ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let storage_inner = storage_block.inner(area);
    frame.render_widget(storage_block, area);

    if storage_inner.height > 0 && storage_inner.width > 0 {
        for (idx, m) in disk_mounts.iter().enumerate() {
            let row = storage_inner.y + idx as u16;
            if row >= storage_inner.bottom() {
                break;
            }
            let label = format!("Mount {}:", m.mount_point);
            let stat = format!(
                "{:.0}% ({}/{})",
                m.used_pct,
                format_bytes_dyn(m.used_bytes as f64),
                format_bytes_dyn(m.total_bytes as f64)
            );
            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: storage_inner.x,
                    y: row,
                    width: storage_inner.width,
                    height: 1,
                },
                &label,
                &stat,
                m.used_pct,
            );
        }
    }
}

/// Renders the General Dashboard (Tab 1).
/// In standard view (when height >= 28 and width >= 70), displays all cards in a grid.
/// In compact / overflow view, provides a tabbed card-by-card browser navigable with arrow keys.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the dashboard.
/// * `sys_info` - Host system overview metadata.
/// * `battery` - Optional laptop battery metrics.
/// * `global_cpu_usage` - Total CPU utilization percentage.
/// * `core_usages` - Per-core CPU load percentages.
/// * `cpu_cur_mhz` - Current CPU clock frequency in MHz.
/// * `cpu_min_mhz` - Minimum CPU clock frequency in MHz.
/// * `cpu_max_mhz` - Maximum CPU clock frequency in MHz.
/// * `cpu_temp` - CPU temperature in degrees Celsius.
/// * `cpu_model` - Marketing CPU model identifier string.
/// * `mem` - Memory metrics.
/// * `ram_info` - DMI memory capacity, speed, and DDR generation string.
/// * `gpu` - GPU metrics.
/// * `net_ifaces` - List of active network interfaces.
/// * `disk_io` - Disk I/O metrics.
/// * `disk_mounts` - Mounted partition list.
/// * `processes` - List of active processes to scan for >30% resource usage.
/// * `copied` - Whether the overview copy button feedback is currently active.
/// * `sub_tab` - Active sub-card index when rendered in compact/overflow mode.
#[allow(clippy::too_many_arguments)]
pub fn render_general_tab(
    frame: &mut Frame,
    area: Rect,
    sys_info: &SystemGeneralInfo,
    battery: Option<&BatteryInfo>,
    global_cpu_usage: f64,
    core_usages: &[f64],
    cpu_cur_mhz: f64,
    cpu_min_mhz: f64,
    cpu_max_mhz: f64,
    cpu_temp: u32,
    cpu_model: &str,
    mem: &MemoryMetrics,
    ram_info: &str,
    gpu: &GpuMetrics,
    net_ifaces: &[NetInterfaceInfo],
    disk_io: &DiskIoInfo,
    disk_mounts: &[MountInfo],
    processes: &[ProcessInfo],
    copied: bool,
    sub_tab: usize,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let num_cores = core_usages.len();
    let cpu_min_rows = 1 + num_cores.div_ceil(4) as u16 + 2;
    let min_height_needed = (14 + (area.height * 30 / 100).max(6) + cpu_min_rows).max(29);
    let is_compact = area.height < min_height_needed || area.width < 80;

    if is_compact {
        let sub_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);

        let titles = vec!["Overview", "Hardware", "High", "Partitions"];
        let tabs = Tabs::new(titles)
            .style(Style::default().fg(Color::Rgb(170, 170, 170)))
            .select(sub_tab % 4)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                    .title(" View (←/→ switch box) ".fg(Color::Rgb(170, 170, 170))),
            )
            .highlight_style(Style::default().fg(Color::Rgb(255, 255, 255)))
            .divider("|");

        frame.render_widget(tabs, sub_chunks[0]);

        let body = sub_chunks[1];
        match sub_tab % 4 {
            0 => {
                if body.width >= 70 {
                    let chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(body);
                    render_general_overview_card(
                        frame, chunks[0], sys_info, cpu_model, num_cores, ram_info, gpu, copied,
                    );
                    render_general_battery_card(frame, chunks[1], battery);
                } else {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(0), Constraint::Length(6)])
                        .split(body);
                    render_general_overview_card(
                        frame, chunks[0], sys_info, cpu_model, num_cores, ram_info, gpu, copied,
                    );
                    render_general_battery_card(frame, chunks[1], battery);
                }
            }
            1 => {
                if body.width >= 70 {
                    let cols = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(body);

                    // Left: CPU taking full height
                    render_general_cpu_card(
                        frame,
                        cols[0],
                        global_cpu_usage,
                        core_usages,
                        cpu_cur_mhz,
                        cpu_min_mhz,
                        cpu_max_mhz,
                        cpu_temp,
                        cpu_model,
                    );

                    // Right: Memory, GPU, Network, IO
                    let right = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(4),
                            Constraint::Length(4),
                            Constraint::Length(4),
                            Constraint::Length(4),
                        ])
                        .split(cols[1]);
                    render_general_memory_card(frame, right[0], mem, ram_info);
                    render_general_gpu_card(frame, right[1], gpu);
                    render_general_net_card(frame, right[2], net_ifaces);
                    render_general_disk_card(frame, right[3], disk_io);
                } else {
                    let rows = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(0),
                            Constraint::Length(4),
                            Constraint::Length(4),
                            Constraint::Length(4),
                            Constraint::Length(4),
                        ])
                        .split(body);
                    render_general_cpu_card(
                        frame,
                        rows[0],
                        global_cpu_usage,
                        core_usages,
                        cpu_cur_mhz,
                        cpu_min_mhz,
                        cpu_max_mhz,
                        cpu_temp,
                        cpu_model,
                    );
                    render_general_memory_card(frame, rows[1], mem, ram_info);
                    render_general_gpu_card(frame, rows[2], gpu);
                    render_general_net_card(frame, rows[3], net_ifaces);
                    render_general_disk_card(frame, rows[4], disk_io);
                }
            }
            2 => {
                render_general_processes_card(frame, body, processes, mem, gpu, num_cores);
            }
            _ => {
                render_general_storage_card(frame, body, disk_mounts);
            }
        }
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(14),
                Constraint::Min(0),
                Constraint::Percentage(30),
            ])
            .split(columns[0]);

        render_general_overview_card(
            frame,
            left_chunks[0],
            sys_info,
            cpu_model,
            num_cores,
            ram_info,
            gpu,
            copied,
        );
        render_general_cpu_card(
            frame,
            left_chunks[1],
            global_cpu_usage,
            core_usages,
            cpu_cur_mhz,
            cpu_min_mhz,
            cpu_max_mhz,
            cpu_temp,
            cpu_model,
        );
        render_general_processes_card(frame, left_chunks[2], processes, mem, gpu, num_cores);

        let battery_height = if battery.is_some() { 7 } else { 5 };
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(battery_height),
                Constraint::Length(4),
                Constraint::Length(4),
                Constraint::Length(4),
                Constraint::Length(4),
                Constraint::Min(0),
            ])
            .split(columns[1]);

        render_general_battery_card(frame, right_chunks[0], battery);
        render_general_memory_card(frame, right_chunks[1], mem, ram_info);
        render_general_gpu_card(frame, right_chunks[2], gpu);
        render_general_net_card(frame, right_chunks[3], net_ifaces);
        render_general_disk_card(frame, right_chunks[4], disk_io);
        render_general_storage_card(frame, right_chunks[5], disk_mounts);
    }
}

/// Renders a modal confirmation dialog when terminating (SIGTERM) or force-killing (SIGKILL) a process.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Full terminal frame bounding box.
/// * `confirm` - Pending process kill confirmation details.
///
/// # Returns
/// A tuple of `(Rect, Rect)` containing the bounding rectangles for the `[ Yes (y) ]` and `[ No (n) ]` clickable buttons.
pub fn render_kill_confirmation_modal(
    frame: &mut Frame,
    area: Rect,
    confirm: &ProcessKillConfirmation,
) -> (Rect, Rect) {
    let popup_width = 58.min(area.width.saturating_sub(4));
    let popup_height = 8.min(area.height.saturating_sub(2));
    let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);

    frame.render_widget(ratatui::widgets::Clear, popup_rect);

    let (title, border_color, sig_text) = if confirm.is_kill {
        (
            " Force Kill (SIGKILL) ",
            Color::Rgb(255, 60, 60),
            "force kill",
        )
    } else {
        (
            " Terminate (SIGTERM) ",
            Color::Rgb(255, 190, 0),
            "terminate",
        )
    };

    let block = Block::default()
        .title(title.fg(border_color).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let target_desc = if confirm.pids.len() > 1 {
        format!(
            "{} ({} processes)",
            confirm.process_name,
            confirm.pids.len()
        )
    } else if let Some(&pid) = confirm.pids.first() {
        format!("{} [PID: {}]", confirm.process_name, pid)
    } else {
        confirm.process_name.clone()
    };

    let question = Line::from(vec![
        Span::raw(" Are you sure you want to "),
        Span::styled(sig_text, Style::default().fg(border_color).bold()),
        Span::raw(" this process?"),
    ]);

    let target_line = Line::from(vec![
        Span::raw("   "),
        Span::styled(
            target_desc,
            Style::default().fg(Color::Rgb(255, 255, 255)).bold(),
        ),
    ]);

    let btn_y = inner.y + inner.height.saturating_sub(2);
    let yes_w = 14;
    let no_w = 14;
    let total_btn_w = yes_w + no_w + 4;
    let btn_start_x = inner.x + inner.width.saturating_sub(total_btn_w) / 2;

    let yes_rect = Rect::new(btn_start_x, btn_y, yes_w, 1);
    let no_rect = Rect::new(btn_start_x + yes_w + 4, btn_y, no_w, 1);

    let lines = vec![question, target_line];

    let paragraph = ratatui::widgets::Paragraph::new(lines);
    frame.render_widget(paragraph, Rect::new(inner.x, inner.y + 1, inner.width, 2));

    // Render Yes Button
    let yes_style = if confirm.is_kill {
        Style::default()
            .bg(Color::Rgb(200, 30, 30))
            .fg(Color::Rgb(255, 255, 255))
            .bold()
    } else {
        Style::default()
            .bg(Color::Rgb(200, 140, 0))
            .fg(Color::Rgb(0, 0, 0))
            .bold()
    };
    frame.render_widget(
        ratatui::widgets::Paragraph::new(" [ Yes (y) ] ")
            .style(yes_style)
            .alignment(ratatui::layout::Alignment::Center),
        yes_rect,
    );

    // Render No Button
    let no_style = Style::default()
        .bg(Color::Rgb(70, 70, 70))
        .fg(Color::Rgb(255, 255, 255))
        .bold();
    frame.render_widget(
        ratatui::widgets::Paragraph::new(" [ No (n) ] ")
            .style(no_style)
            .alignment(ratatui::layout::Alignment::Center),
        no_rect,
    );

    (yes_rect, no_rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::DiskDeviceInfo;

    #[test]
    fn test_is_disks_overflow() {
        let area_small = Rect::new(0, 0, 80, 20);
        let area_large = Rect::new(0, 0, 160, 50);

        let disk_io = DiskIoInfo {
            read_speed: 1024.0,
            write_speed: 2048.0,
            total_read_bytes: 100000,
            total_write_bytes: 200000,
            disks: vec![DiskDeviceInfo {
                name: "nvme0n1".to_string(),
                model: "Samsung SSD 980 PRO 1TB".to_string(),
                read_speed: 1024.0,
                write_speed: 2048.0,
            }],
        };

        let mounts = vec![
            MountInfo {
                mount_point: "/".to_string(),
                device: "/dev/nvme0n1p2".to_string(),
                fs_type: "ext4".to_string(),
                total_bytes: 500_000_000_000,
                used_bytes: 200_000_000_000,
                free_bytes: 300_000_000_000,
                used_pct: 40.0,
            },
            MountInfo {
                mount_point: "/home".to_string(),
                device: "/dev/nvme0n1p3".to_string(),
                fs_type: "ext4".to_string(),
                total_bytes: 500_000_000_000,
                used_bytes: 200_000_000_000,
                free_bytes: 300_000_000_000,
                used_pct: 40.0,
            },
        ];

        // On small area (width 80 -> top_right_w is 22 cols, disk model is 34 chars), it should overflow
        assert!(is_disks_overflow(area_small, &disk_io, &mounts));

        // On large area (width 160, height 50), it fits comfortably without overflow
        assert!(!is_disks_overflow(area_large, &disk_io, &mounts));
    }

    #[test]
    fn test_is_gpu_overflow() {
        let area_small = Rect::new(0, 0, 100, 30);
        let area_large = Rect::new(0, 0, 200, 40);

        let gpu_long = GpuMetrics {
            name: "NVIDIA GeForce RTX 4090".to_string(),
            vram_total_mb: 24576,
            ..Default::default()
        };

        let gpu_short = GpuMetrics {
            name: "GPU".to_string(),
            vram_total_mb: 0,
            ..Default::default()
        };

        // Long GPU model (30 chars) on width 100 (graph_w = 30) overflows (needs >= 49)
        assert!(is_gpu_overflow(area_small, &gpu_long));

        // Short GPU model on width 100 (graph_w = 30 >= 23) fits without overflow
        assert!(!is_gpu_overflow(area_small, &gpu_short));

        // On large area (width 200 -> graph_w = 60 >= 49), long GPU model fits without overflow
        assert!(!is_gpu_overflow(area_large, &gpu_long));
    }
}

