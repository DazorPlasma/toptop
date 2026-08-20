//! User interface rendering engine.
//!
//! Provides Ratatui terminal UI rendering components, including high-resolution
//! 2x4 sub-pixel Unicode Braille gradient historical graphs, tables, gauges,
//! and dashboard cards for all 6 monitor tabs.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Cell, Row, Table, TableState},
};

use crate::{
    process::{ProcessInfo, ProcessSortColumn},
    system::{
        BatteryInfo, DiskIoInfo, GpuMetrics, MemoryMetrics, MountInfo, NetInterfaceInfo,
        PackageStorageCategory, SystemGeneralInfo,
    },
    theme::{darken_color, gradient_color, io_gradient_pct, process_cpu_color},
    utils::{format_bytes_dyn, format_freq, format_percent, format_uptime, fuzzy_match},
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
/// * `center_title` - Optional centered header tag and styling.
/// * `right_title` - Optional right-aligned header tag text.
/// * `border_color` - Border line color.
/// * `history` - Slice of optional data samples (0.0 to 100.0) where `None` indicates uncollected startup state.
pub fn render_gradient_chart(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    center_title: Option<(&str, Color)>,
    right_title: Option<&str>,
    border_color: Color,
    history: &[Option<f64>],
) {
    let mut block = Block::default()
        .title(title.fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color));

    if let Some((ct, color)) = center_title
        && !ct.is_empty()
    {
        block = block.title(
            Line::from(format!(" {} ", ct).fg(color).bold())
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

/// Renders the Process Manager (Tab 2) interactive table view with sorting, filtering, and detailed columns.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the table widget.
/// * `processes` - List of active processes to display.
/// * `advanced_view` - Whether advanced column mode (with UID, State, Threads, etc.) is enabled.
/// * `search_query` - Current fuzzy filter search text.
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
    search_query: &str,
    is_searching: bool,
    current_sort_col: ProcessSortColumn,
    sort_ascending: bool,
    total_mem: u64,
    num_cores: usize,
    table_state: &mut TableState,
) {
    let displayed_processes: Vec<&ProcessInfo> = processes
        .iter()
        .filter(|p| advanced_view || p.rss_kb > 0)
        .filter(|p| {
            search_query.is_empty()
                || fuzzy_match(search_query, &p.name)
                || fuzzy_match(search_query, &p.pid.to_string())
                || fuzzy_match(search_query, &p.user)
        })
        .collect();

    let (header_titles, constraints): (Vec<(&str, ProcessSortColumn)>, Vec<Constraint>) =
        if advanced_view {
            (
                vec![
                    ("PID", ProcessSortColumn::Pid),
                    ("User", ProcessSortColumn::User),
                    ("Name", ProcessSortColumn::Name),
                    ("State", ProcessSortColumn::State),
                    ("Threads", ProcessSortColumn::Threads),
                    ("CPU", ProcessSortColumn::Cpu),
                    ("RAM", ProcessSortColumn::Mem),
                    ("GPU", ProcessSortColumn::Gpu),
                    ("VRAM", ProcessSortColumn::GpuMem),
                    ("IO", ProcessSortColumn::Io),
                    ("Net", ProcessSortColumn::Net),
                ],
                vec![
                    Constraint::Length(8),  // PID
                    Constraint::Length(10), // User
                    Constraint::Fill(1),    // Name
                    Constraint::Length(7),  // State
                    Constraint::Length(8),  // Threads
                    Constraint::Length(8),  // CPU
                    Constraint::Length(10), // RAM
                    Constraint::Length(8),  // GPU
                    Constraint::Length(10), // VRAM
                    Constraint::Length(23), // IO (read / write)
                    Constraint::Length(23), // Net (down / up)
                ],
            )
        } else {
            (
                vec![
                    ("PID", ProcessSortColumn::Pid),
                    ("Name", ProcessSortColumn::Name),
                    ("CPU", ProcessSortColumn::Cpu),
                    ("RAM", ProcessSortColumn::Mem),
                    ("GPU", ProcessSortColumn::Gpu),
                    ("VRAM", ProcessSortColumn::GpuMem),
                    ("IO", ProcessSortColumn::Io),
                    ("Net", ProcessSortColumn::Net),
                ],
                vec![
                    Constraint::Length(8),  // PID
                    Constraint::Fill(1),    // Name
                    Constraint::Length(8),  // CPU
                    Constraint::Length(10), // RAM
                    Constraint::Length(8),  // GPU
                    Constraint::Length(10), // VRAM
                    Constraint::Length(23), // IO (read / write)
                    Constraint::Length(23), // Net (down / up)
                ],
            )
        };

    let header_cells = header_titles.iter().map(|&(title, col)| {
        if col == current_sort_col {
            let arrow = if sort_ascending { "▲" } else { "▼" };
            Cell::from(format!("{} {}", title, arrow))
                .style(Style::default().fg(Color::Rgb(255, 255, 0)).bold())
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

        let cells = if advanced_view {
            vec![
                Cell::from(p.pid.to_string()).style(Style::default().fg(text_color)),
                Cell::from(p.user.clone()).style(Style::default().fg(text_color)),
                Cell::from(p.name.clone()).style(Style::default().fg(text_color)),
                Cell::from(p.state.clone()).style(Style::default().fg(text_color)),
                Cell::from(p.threads.to_string()).style(Style::default().fg(text_color)),
                Cell::from(format_percent(p.cpu_percent)).style(Style::default().fg(cpu_color)),
                Cell::from(format_bytes_dyn((p.rss_kb * 1024) as f64))
                    .style(Style::default().fg(mem_color)),
                Cell::from(format_percent(p.gpu_percent)).style(Style::default().fg(gpu_color)),
                Cell::from(format_bytes_dyn((p.gpu_mem_kb * 1024) as f64))
                    .style(Style::default().fg(gpu_mem_color)),
                Cell::from(io_text).style(Style::default().fg(io_color)),
                Cell::from(net_text).style(Style::default().fg(net_color)),
            ]
        } else {
            vec![
                Cell::from(p.pid.to_string()).style(Style::default().fg(text_color)),
                Cell::from(p.name.clone()).style(Style::default().fg(text_color)),
                Cell::from(format_percent(p.cpu_percent)).style(Style::default().fg(cpu_color)),
                Cell::from(format_bytes_dyn((p.rss_kb * 1024) as f64))
                    .style(Style::default().fg(mem_color)),
                Cell::from(format_percent(p.gpu_percent)).style(Style::default().fg(gpu_color)),
                Cell::from(format_bytes_dyn((p.gpu_mem_kb * 1024) as f64))
                    .style(Style::default().fg(gpu_mem_color)),
                Cell::from(io_text).style(Style::default().fg(io_color)),
                Cell::from(net_text).style(Style::default().fg(net_color)),
            ]
        };
        Row::new(cells).height(1)
    });

    let table_title = if is_searching {
        format!(
            " Search: {}_ (Enter/Esc to finish, 'c' Term, 'k' Kill) ",
            search_query
        )
    } else if !search_query.is_empty() {
        let mode_str = if advanced_view { "Advanced, " } else { "" };
        format!(
            " Processes [{}Filter: \"{}\" - {} matches] ('/' Search, Esc Clear, ←/→ Sort, 'r' Order, 'c' Term, 'k' Kill, ↑/↓/g/G Select) ",
            mode_str,
            search_query,
            displayed_processes.len()
        )
    } else if advanced_view {
        " Processes [Advanced] ('/' Search, ←/→ Sort, 'r' Order, 'c' Term, 'k' Kill, ↑/↓/g/G Select, 'a' Normal View) "
            .to_string()
    } else {
        " Processes ('/' Search, ←/→ Sort, 'r' Order, 'c' Term, 'k' Kill, ↑/↓/g/G Select, 'a' Advanced View) "
            .to_string()
    };

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
    ram_info: &str,
) {
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let cpu_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Percentage(20),
            Constraint::Length(9),
        ])
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
        Some((&cpu_freq_label, cpu_freq_color)),
        Some(cpu_model),
        Color::Rgb(60, 60, 60),
        cpu_history,
    );

    let cores_block = Block::default()
        .title("Cores".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let cores_inner = cores_block.inner(cpu_chunks[1]);
    frame.render_widget(cores_block, cpu_chunks[1]);

    let cores_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Ratio(1, core_usages.len().max(1) as u32);
            core_usages.len().max(1)
        ])
        .split(cores_inner);

    for (i, &usage) in core_usages.iter().enumerate() {
        if i >= cores_layout.len() {
            break;
        }
        let c_area = cores_layout[i];
        if c_area.height == 0 || c_area.width == 0 {
            continue;
        }
        let label = format!("C{:<2} {:>3.0}% ", i, usage);
        let label_len = label.chars().count() as u16;
        for (idx, ch) in label.chars().enumerate() {
            let col = c_area.x + idx as u16;
            if col < c_area.right() {
                frame.buffer_mut()[(col, c_area.y)]
                    .set_char(ch)
                    .set_style(Style::default().fg(Color::Rgb(170, 170, 170)));
            }
        }

        let bar_start = c_area.x + label_len;
        let total_bar = c_area.right().saturating_sub(bar_start);
        let usage_clamped = usage.clamp(0.0, 100.0);
        let filled_len = ((total_bar as f64) * (usage_clamped / 100.0)).round() as u16;

        for c in 0..total_bar {
            let col = bar_start + c;
            if col >= c_area.right() {
                break;
            }
            let pct = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
            if c < filled_len {
                let color = gradient_color(pct);
                frame.buffer_mut()[(col, c_area.y)]
                    .set_char('━')
                    .set_style(Style::default().fg(color));
            } else {
                frame.buffer_mut()[(col, c_area.y)]
                    .set_char('─')
                    .set_style(Style::default().fg(Color::Rgb(50, 50, 50)));
            }
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

    let mem_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(body_chunks[1]);

    let mem_percent_f64 = if mem.total_mem_mb > 0 {
        (mem.used_mem_mb as f64 / mem.total_mem_mb as f64) * 100.0
    } else {
        0.0
    };

    render_gradient_chart(
        frame,
        mem_layout[0],
        "Memory History",
        None,
        Some(ram_info),
        Color::Rgb(60, 60, 60),
        mem_history,
    );

    let mem_block = Block::default()
        .title("Memory Usage".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let mem_inner = mem_block.inner(mem_layout[1]);
    frame.render_widget(mem_block, mem_layout[1]);

    if mem_inner.height > 0 && mem_inner.width > 0 {
        let total_bar = mem_inner.width;
        let filled_len =
            ((total_bar as f64) * (mem_percent_f64.clamp(0.0, 100.0) / 100.0)).round() as u16;
        let label = format!("{} MB / {} MB", mem.used_mem_mb, mem.total_mem_mb);
        let label_chars: Vec<char> = label.chars().collect();
        let label_start = (total_bar.saturating_sub(label_chars.len() as u16)) / 2;

        for c in 0..total_bar {
            let col = mem_inner.x + c;
            let row = mem_inner.y;
            let pct = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
            let is_filled = c < filled_len;
            let bar_color = gradient_color(pct);

            if c >= label_start && (c - label_start) < label_chars.len() as u16 {
                let ch = label_chars[(c - label_start) as usize];
                if is_filled {
                    frame.buffer_mut()[(col, row)].set_char(ch).set_style(
                        Style::default()
                            .fg(Color::Rgb(0, 0, 0))
                            .bg(bar_color)
                            .bold(),
                    );
                } else {
                    frame.buffer_mut()[(col, row)].set_char(ch).set_style(
                        Style::default()
                            .fg(Color::Rgb(170, 170, 170))
                            .bg(Color::Rgb(30, 30, 30)),
                    );
                }
            } else if is_filled {
                frame.buffer_mut()[(col, row)]
                    .set_char('█')
                    .set_style(Style::default().fg(bar_color));
            } else {
                frame.buffer_mut()[(col, row)]
                    .set_char(' ')
                    .set_style(Style::default().bg(Color::Rgb(30, 30, 30)));
            }
        }
    }

    let swap_block = Block::default()
        .title("Swap Usage".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let swap_inner = swap_block.inner(mem_layout[2]);
    frame.render_widget(swap_block, mem_layout[2]);

    if swap_inner.height > 0 && swap_inner.width > 0 {
        let swap_pct = if mem.total_swap_mb > 0 {
            (mem.used_swap_mb as f64 / mem.total_swap_mb as f64) * 100.0
        } else {
            0.0
        };
        let total_bar = swap_inner.width;
        let filled_len = ((total_bar as f64) * (swap_pct.clamp(0.0, 100.0) / 100.0)).round() as u16;
        let label = if mem.total_swap_mb > 0 {
            format!("{} MB / {} MB", mem.used_swap_mb, mem.total_swap_mb)
        } else {
            "0 MB / 0 MB (No Swap Configured)".to_string()
        };
        let label_chars: Vec<char> = label.chars().collect();
        let label_start = (total_bar.saturating_sub(label_chars.len() as u16)) / 2;

        for c in 0..total_bar {
            let col = swap_inner.x + c;
            let row = swap_inner.y;
            let pct = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
            let is_filled = c < filled_len;
            let bar_color = gradient_color(pct);

            if c >= label_start && (c - label_start) < label_chars.len() as u16 {
                let ch = label_chars[(c - label_start) as usize];
                if is_filled {
                    frame.buffer_mut()[(col, row)].set_char(ch).set_style(
                        Style::default()
                            .fg(Color::Rgb(0, 0, 0))
                            .bg(bar_color)
                            .bold(),
                    );
                } else {
                    frame.buffer_mut()[(col, row)].set_char(ch).set_style(
                        Style::default()
                            .fg(Color::Rgb(170, 170, 170))
                            .bg(Color::Rgb(30, 30, 30)),
                    );
                }
            } else if is_filled {
                frame.buffer_mut()[(col, row)]
                    .set_char('█')
                    .set_style(Style::default().fg(bar_color));
            } else {
                frame.buffer_mut()[(col, row)]
                    .set_char(' ')
                    .set_style(Style::default().bg(Color::Rgb(30, 30, 30)));
            }
        }
    }
}

/// Renders the GPU (Tab 4) utilization graph, VRAM graph, core/memory clocks, and temperatures.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the tab.
/// * `gpu_metrics` - Current GPU metrics and status.
/// * `gpu_history` - Historical GPU core busy percentages for Braille graphing.
/// * `gpu_vram_history` - Historical GPU VRAM consumption for Braille graphing.
pub fn render_gpu_tab(
    frame: &mut Frame,
    area: Rect,
    gpu_metrics: &GpuMetrics,
    gpu_history: &[Option<f64>],
    gpu_vram_history: &[Option<f64>],
) {
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(9)])
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

    render_gradient_chart(
        frame,
        top_chunks[0],
        "GPU Utilization History",
        Some((&gpu_freq_label, gpu_freq_color)),
        Some(&gpu_label),
        Color::Rgb(60, 60, 60),
        gpu_history,
    );

    let temp_block = Block::default()
        .title(" Temp ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let temp_inner = temp_block.inner(top_chunks[1]);
    frame.render_widget(temp_block, top_chunks[1]);

    if temp_inner.height > 0 && temp_inner.width > 0 {
        let temp_pct = ((gpu_metrics.temp_c as f64 - 25.0) / 75.0 * 100.0).clamp(0.0, 100.0);
        let temp_color = gradient_color(temp_pct);

        let temp_str = format!("{}°C", gpu_metrics.temp_c);
        let str_len = temp_str.chars().count() as u16;
        let str_start = temp_inner.x + (temp_inner.width.saturating_sub(str_len)) / 2;

        for (idx, ch) in temp_str.chars().enumerate() {
            let col = str_start + idx as u16;
            if col < temp_inner.right() {
                frame.buffer_mut()[(col, temp_inner.y)]
                    .set_char(ch)
                    .set_style(Style::default().fg(temp_color).bold());
            }
        }

        if temp_inner.height > 2 {
            let bar_height = temp_inner.height - 2;
            let filled_rows = ((bar_height as f64) * (temp_pct / 100.0)).round() as u16;
            let bar_width = 3.min(temp_inner.width);
            let bar_x_start = temp_inner.x + (temp_inner.width.saturating_sub(bar_width)) / 2;

            for r in 0..bar_height {
                let row_y = temp_inner.bottom() - 1 - r;
                let row_pct = (r as f64 + 0.5) / (bar_height as f64) * 100.0;
                let is_filled = r < filled_rows;
                let color = gradient_color(row_pct);

                for c in 0..bar_width {
                    let col = bar_x_start + c;
                    if col < temp_inner.right() && row_y < temp_inner.bottom() {
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

    let vram_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(body_chunks[1]);

    let vram_pct = if gpu_metrics.vram_total_mb > 0 {
        (gpu_metrics.vram_used_mb as f64 / gpu_metrics.vram_total_mb as f64) * 100.0
    } else {
        0.0
    };

    let vram_label = format!(
        "{} MB / {} MB",
        gpu_metrics.vram_used_mb, gpu_metrics.vram_total_mb
    );
    render_gradient_chart(
        frame,
        vram_layout[0],
        "VRAM History",
        None,
        Some(&vram_label),
        Color::Rgb(60, 60, 60),
        gpu_vram_history,
    );

    let vram_block = Block::default()
        .title(" VRAM Usage ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let vram_inner = vram_block.inner(vram_layout[1]);
    frame.render_widget(vram_block, vram_layout[1]);

    if vram_inner.height > 0 && vram_inner.width > 0 {
        let total_bar = vram_inner.width;
        let filled_len = ((total_bar as f64) * (vram_pct.clamp(0.0, 100.0) / 100.0)).round() as u16;
        let label = format!(
            "{} MB / {} MB",
            gpu_metrics.vram_used_mb, gpu_metrics.vram_total_mb
        );
        let label_chars: Vec<char> = label.chars().collect();
        let label_start = (total_bar.saturating_sub(label_chars.len() as u16)) / 2;

        for c in 0..total_bar {
            let col = vram_inner.x + c;
            let row = vram_inner.y;
            let pct = (c as f64 + 0.5) / (total_bar as f64) * 100.0;
            let is_filled = c < filled_len;
            let bar_color = gradient_color(pct);

            if c >= label_start && (c - label_start) < label_chars.len() as u16 {
                let ch = label_chars[(c - label_start) as usize];
                if is_filled {
                    frame.buffer_mut()[(col, row)].set_char(ch).set_style(
                        Style::default()
                            .fg(Color::Rgb(0, 0, 0))
                            .bg(bar_color)
                            .bold(),
                    );
                } else {
                    frame.buffer_mut()[(col, row)].set_char(ch).set_style(
                        Style::default()
                            .fg(Color::Rgb(170, 170, 170))
                            .bg(Color::Rgb(30, 30, 30)),
                    );
                }
            } else if is_filled {
                frame.buffer_mut()[(col, row)]
                    .set_char('█')
                    .set_style(Style::default().fg(bar_color));
            } else {
                frame.buffer_mut()[(col, row)]
                    .set_char(' ')
                    .set_style(Style::default().bg(Color::Rgb(30, 30, 30)));
            }
        }
    }
}

/// Renders the Network (Tab 5) download/upload graphs, primary interface card, and interfaces list.
///
/// # Arguments
/// * `frame` - Terminal rendering frame buffer.
/// * `area` - Target bounding box for the tab.
/// * `net_ifaces` - List of detected network interfaces and throughput rates.
/// * `net_rx_history` - Historical download (RX) speed samples for Braille graphing.
/// * `net_tx_history` - Historical upload (TX) speed samples for Braille graphing.
pub fn render_network_tab(
    frame: &mut Frame,
    area: Rect,
    net_ifaces: &[NetInterfaceInfo],
    net_rx_history: &[Option<f64>],
    net_tx_history: &[Option<f64>],
) {
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(body_chunks[0]);

    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(body_chunks[1]);

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
        top_chunks[0],
        "Download (RX) History",
        None,
        Some(&rx_label),
        Color::Rgb(60, 60, 60),
        net_rx_history,
    );

    let iface_block = Block::default()
        .title(" Primary Interface ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let iface_inner = iface_block.inner(top_chunks[1]);
    frame.render_widget(iface_block, top_chunks[1]);

    if iface_inner.height > 0
        && iface_inner.width > 0
        && let Some(iface) = primary_iface
    {
        let lines = [
            format!("Interface: {}", iface.name),
            format!("Status:    {}", iface.operstate),
            format!("Speed:     {} Mbps", iface.speed_mbps),
            format!("Duplex:    {}", iface.duplex),
            format!("MAC:       {}", iface.mac),
            format!("Total RX:  {}", format_bytes_dyn(iface.rx_bytes as f64)),
            format!("Total TX:  {}", format_bytes_dyn(iface.tx_bytes as f64)),
        ];
        for (idx, line_str) in lines.iter().enumerate() {
            let row = iface_inner.y + idx as u16;
            if row < iface_inner.bottom() {
                for (c_idx, ch) in line_str.chars().enumerate() {
                    let col = iface_inner.x + c_idx as u16;
                    if col < iface_inner.right() {
                        let color = if idx == 1 && iface.operstate == "up" {
                            Color::Rgb(0, 255, 0)
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

    render_gradient_chart(
        frame,
        bottom_chunks[0],
        "Upload (TX) History",
        None,
        Some(&tx_label),
        Color::Rgb(60, 60, 60),
        net_tx_history,
    );

    let list_block = Block::default()
        .title(" All Interfaces ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let list_inner = list_block.inner(bottom_chunks[1]);
    frame.render_widget(list_block, bottom_chunks[1]);

    if list_inner.height > 0 && list_inner.width > 0 {
        let mut row_offset = 0;
        for iface in net_ifaces {
            let line1 = format!("{}: {}", iface.name, iface.operstate);
            let line2 = format!(
                " {} ↓ / {} ↑",
                format_bytes_dyn(iface.rx_speed),
                format_bytes_dyn(iface.tx_speed)
            );
            for line_str in &[line1, line2] {
                let row = list_inner.y + row_offset;
                if row < list_inner.bottom() {
                    for (c_idx, ch) in line_str.chars().enumerate() {
                        let col = list_inner.x + c_idx as u16;
                        if col < list_inner.right() {
                            frame.buffer_mut()[(col, row)]
                                .set_char(ch)
                                .set_style(Style::default().fg(Color::Rgb(170, 170, 170)));
                        }
                    }
                }
                row_offset += 1;
            }
        }
    }
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
) {
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(body_chunks[0]);

    // Fit both graphs in the space of the top-left area
    let graph_chunks = Layout::default()
        .direction(Direction::Vertical)
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

    let disks_block = Block::default()
        .title(" Physical Disks ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let disks_inner = disks_block.inner(top_chunks[1]);
    frame.render_widget(disks_block, top_chunks[1]);

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
                        let color = if idx == 0 && disk_io.read_speed > 0.0 {
                            gradient_color(io_gradient_pct(disk_io.read_speed))
                        } else if idx == 1 && disk_io.write_speed > 0.0 {
                            gradient_color(io_gradient_pct(disk_io.write_speed))
                        } else if idx == 5 {
                            Color::Rgb(255, 255, 255)
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

    // Bottom-Left: Package & Application Storage Sub-Tabs
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
    let pkg_inner = pkg_block.inner(bottom_chunks[0]);
    frame.render_widget(pkg_block, bottom_chunks[0]);

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
            let start_idx = scroll_offset.min(total_items.saturating_sub(1));

            // 1. Render Sub-Tabs Header Line
            let mut tab_col = pkg_inner.x;
            for (i, cat) in storage_categories.iter().enumerate() {
                let is_active = i == cat_idx;
                let tab_label = if is_active {
                    format!("[ ▶ {} ({}) ] ", cat.name, cat.total_str)
                } else {
                    format!("[ {} ({}) ] ", cat.name, cat.total_str)
                };

                let style = if is_active {
                    Style::default()
                        .fg(Color::Rgb(255, 255, 255))
                        .bold()
                        .bg(Color::Rgb(40, 40, 40))
                } else {
                    Style::default().fg(Color::Rgb(130, 130, 130))
                };

                for ch in tab_label.chars() {
                    if tab_col < pkg_inner.right() {
                        frame.buffer_mut()[(tab_col, pkg_inner.y)]
                            .set_char(ch)
                            .set_style(style);
                        tab_col += 1;
                    }
                }
            }

            // 2. Items list with scroll offset
            let max_item_bytes = active_cat
                .items
                .iter()
                .map(|it| it.size_bytes)
                .max()
                .unwrap_or(1)
                .max(1);

            let mut row_offset = 2;
            for item in active_cat.items.iter().skip(start_idx) {
                if row_offset + 2 > pkg_inner.height {
                    break;
                }
                let display_name = if item.name.len() > 42 {
                    format!("{}…", &item.name[..41])
                } else {
                    item.name.clone()
                };
                let header = if item.detail.is_empty() {
                    format!("• {}", display_name)
                } else {
                    format!("• {} [{}]", display_name, item.detail)
                };
                let row_h = pkg_inner.y + row_offset;
                let row_b = row_h + 1;

                for (c_idx, ch) in header.chars().enumerate() {
                    let col = pkg_inner.x + c_idx as u16;
                    if col < pkg_inner.right() {
                        frame.buffer_mut()[(col, row_h)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(240, 240, 240)).bold());
                    }
                }

                if item.size_bytes > 0 {
                    let item_pct = (item.size_bytes as f64 / max_item_bytes as f64) * 100.0;
                    let right_label = item.size_str.clone();
                    let bar_area =
                        Rect::new(pkg_inner.x + 2, row_b, pkg_inner.width.saturating_sub(2), 1);
                    draw_labeled_bar(
                        frame.buffer_mut(),
                        bar_area,
                        "Size: ",
                        &right_label,
                        item_pct,
                    );
                    row_offset += 3;
                } else {
                    let right_label = item.size_str.clone();
                    let row_sub = row_h;
                    let r_col = pkg_inner
                        .right()
                        .saturating_sub(right_label.len() as u16 + 1);
                    for (c_idx, ch) in right_label.chars().enumerate() {
                        let col = r_col + c_idx as u16;
                        if col < pkg_inner.right() {
                            frame.buffer_mut()[(col, row_sub)]
                                .set_char(ch)
                                .set_style(Style::default().fg(Color::Rgb(140, 140, 140)));
                        }
                    }
                    row_offset += 2;
                }
            }
        }
    }

    // Bottom-Right: Mounted Filesystems
    let mounts_block = Block::default()
        .title(" Mounted Filesystems ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let mounts_inner = mounts_block.inner(bottom_chunks[1]);
    frame.render_widget(mounts_block, bottom_chunks[1]);

    if mounts_inner.height > 0 && mounts_inner.width > 0 {
        let mut row_offset = 0;
        for m in disk_mounts {
            if row_offset + 2 > mounts_inner.height {
                break;
            }
            let header = format!("{} [{}] ({})", m.mount_point, m.device, m.fs_type);
            let sub = format!(
                "{} / {} (Free: {})",
                format_bytes_dyn(m.used_bytes as f64),
                format_bytes_dyn(m.total_bytes as f64),
                format_bytes_dyn(m.free_bytes as f64)
            );
            let row1 = mounts_inner.y + row_offset;
            let row2 = row1 + 1;

            for (c_idx, ch) in header.chars().enumerate() {
                let col = mounts_inner.x + c_idx as u16;
                if col < mounts_inner.right() {
                    frame.buffer_mut()[(col, row1)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(255, 255, 255)).bold());
                }
            }

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
                    frame.buffer_mut()[(col, row2)]
                        .set_char('━')
                        .set_style(Style::default().fg(color));
                } else {
                    frame.buffer_mut()[(col, row2)]
                        .set_char('─')
                        .set_style(Style::default().fg(Color::Rgb(50, 50, 50)));
                }
            }

            row_offset += 2;
            if row_offset < mounts_inner.height {
                let row3 = mounts_inner.y + row_offset;
                for (c_idx, ch) in sub.chars().enumerate() {
                    let col = mounts_inner.x + c_idx as u16;
                    if col < mounts_inner.right() {
                        frame.buffer_mut()[(col, row3)]
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
        &[(label_right, Color::Rgb(220, 220, 220), true)],
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

    lines.push(format!(
        "Load:     {:.2}, {:.2}, {:.2} (1m, 5m, 15m)",
        sys_info.load_1, sys_info.load_5, sys_info.load_15
    ));

    lines.join("\n")
}

/// Renders the General Dashboard (Tab 1) featuring the full System Overview card,
/// battery drainage/charging metrics, unmerged network/disk cards, and high-resource processes list.
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
) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(15), Constraint::Min(0)])
        .split(area);

    // 1. Top Section: System Info & Battery Cards
    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_chunks[0]);

    let sys_block = Block::default()
        .title(" System Overview ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let sys_inner = sys_block.inner(top_chunks[0]);
    frame.render_widget(sys_block, top_chunks[0]);

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

        lines.push(format!(
            "Load:     {:.2}, {:.2}, {:.2} (1m, 5m, 15m)",
            sys_info.load_1, sys_info.load_5, sys_info.load_15
        ));

        for (idx, line) in lines.iter().enumerate() {
            let row = sys_inner.y + idx as u16;
            if row < sys_inner.bottom() {
                let label_end = line.find(':').map(|p| p + 1).unwrap_or(0);
                for (c_idx, ch) in line.chars().enumerate() {
                    let col = sys_inner.x + c_idx as u16;
                    if col < sys_inner.right() {
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
                Style::default().fg(Color::Rgb(0, 255, 128)).bold()
            } else {
                Style::default().fg(Color::Rgb(100, 200, 255)).bold()
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

    let pwr_block = Block::default()
        .title(" Battery & Power ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let pwr_inner = pwr_block.inner(top_chunks[1]);
    frame.render_widget(pwr_block, top_chunks[1]);

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
            let pwr_line = "Power Source: AC Connected (Desktop System / No Battery)";
            for (c_idx, ch) in pwr_line.chars().enumerate() {
                let col = pwr_inner.x + c_idx as u16;
                if col < pwr_inner.right() {
                    frame.buffer_mut()[(col, pwr_inner.y)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(0, 255, 128)).bold());
                }
            }
            let pwr_sub_lines = [
                "Hardware sensors indicate direct wall power.",
                "Power State: Active & Stable",
            ];
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

    // 2. Middle & Lower Section: Full Bar Charts from Everywhere
    let body_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_chunks[1]);

    // Left Column: CPU, Memory, Heavy Processes
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(25),
            Constraint::Percentage(35),
        ])
        .split(body_columns[0]);

    // CPU Card
    let cpu_block = Block::default()
        .title(" CPU Performance ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let cpu_inner = cpu_block.inner(left_chunks[0]);
    frame.render_widget(cpu_block, left_chunks[0]);

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
        let cpu_model_str = format!(", {}]", cpu_model);

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
                (&cpu_pct_str, Color::Rgb(220, 220, 220), true),
                (&cpu_freq_str, cpu_freq_color, true),
                (", ", Color::Rgb(170, 170, 170), false),
                (&cpu_temp_str, cpu_temp_color, true),
                (&cpu_model_str, Color::Rgb(170, 170, 170), false),
            ],
        );

        let num_cores = core_usages.len();
        let half = num_cores.div_ceil(2);

        for (row_idx, i) in (2u16..).zip(0..half) {
            let row_y = cpu_inner.y + row_idx;
            if row_y >= cpu_inner.bottom() {
                break;
            }

            // Left core
            let c1 = i;
            let u1 = core_usages.get(c1).copied().unwrap_or(0.0);
            let w_half = (cpu_inner.width.saturating_sub(2)) / 2;

            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: cpu_inner.x,
                    y: row_y,
                    width: w_half,
                    height: 1,
                },
                &format!("C{:<2}:", c1),
                &format!("{:.0}%", u1),
                u1,
            );

            // Right core
            let c2 = i + half;
            if let Some(&u2) = core_usages.get(c2) {
                draw_labeled_bar(
                    frame.buffer_mut(),
                    Rect {
                        x: cpu_inner.x + w_half + 2,
                        y: row_y,
                        width: w_half,
                        height: 1,
                    },
                    &format!("C{:<2}:", c2),
                    &format!("{:.0}%", u2),
                    u2,
                );
            }
        }
    }

    // Memory Card
    let mem_block = Block::default()
        .title(" Memory & Swap ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let mem_inner = mem_block.inner(left_chunks[1]);
    frame.render_widget(mem_block, left_chunks[1]);

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
                "{:.1}% ({:.1}/{:.1} GB) [{}]",
                ram_pct, ram_used_gb, ram_total_gb, ram_info
            ),
            ram_pct,
        );

        if mem_inner.height > 2 {
            let swap_pct = if mem.total_swap_mb > 0 {
                (mem.used_swap_mb as f64 / mem.total_swap_mb as f64) * 100.0
            } else {
                0.0
            };
            let swap_used_gb = mem.used_swap_mb as f64 / 1024.0;
            let swap_total_gb = mem.total_swap_mb as f64 / 1024.0;

            draw_labeled_bar(
                frame.buffer_mut(),
                Rect {
                    x: mem_inner.x,
                    y: mem_inner.y + 2,
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

    // Heavy Processes (>30%) Card
    let heavy_block = Block::default()
        .title(" High Resource Processes (>30%) ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let heavy_inner = heavy_block.inner(left_chunks[2]);
    frame.render_widget(heavy_block, left_chunks[2]);

    if heavy_inner.height > 0 && heavy_inner.width > 0 {
        let mut heavy_procs: Vec<(&ProcessInfo, f64, f64, f64)> = processes
            .iter()
            .filter_map(|p| {
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
                let max_pct = p
                    .cpu_percent
                    .max(mem_pct)
                    .max(p.gpu_percent)
                    .max(gpu_vram_pct);
                if max_pct >= 30.0 {
                    Some((p, mem_pct, gpu_vram_pct, max_pct))
                } else {
                    None
                }
            })
            .collect();

        heavy_procs.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));

        if heavy_procs.is_empty() {
            let msg = "No processes exceeding 30% resource threshold";
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
            let hdr_user = "USER";

            let name_w =
                (heavy_inner.width.saturating_sub(7 + 8 + 8 + 8 + 8 + 10 + 2) as usize).max(8);
            let header_str = format!(
                "{:<7}{:<name_w$} {:>7} {:>7} {:>7} {:>7}  {:<8}",
                hdr_pid,
                hdr_name,
                hdr_cpu,
                hdr_mem,
                hdr_gpu,
                hdr_vram,
                hdr_user,
                name_w = name_w
            );

            for (c_idx, ch) in header_str.chars().enumerate() {
                let col = heavy_inner.x + c_idx as u16;
                if col < heavy_inner.right() {
                    frame.buffer_mut()[(col, heavy_inner.y)]
                        .set_char(ch)
                        .set_style(Style::default().fg(Color::Rgb(200, 200, 200)).bold());
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
                let name_col_str = format!("{:<name_w$} ", name_display, name_w = name_w);
                let cpu_val_str = format!("{:>6.1}% ", p.cpu_percent);
                let mem_val_str = format!("{:>6.1}% ", mem_pct);
                let gpu_val_str = format!("{:>6.1}% ", p.gpu_percent);
                let vram_val_str = format!("{:>6.1}% ", gpu_vram_pct);
                let user_val_str = format!("{:<8}", p.user);

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
                for ch in name_col_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(220, 220, 220)));
                        col += 1;
                    }
                }
                // CPU%
                let cpu_color = if p.cpu_percent >= 30.0 {
                    gradient_color(p.cpu_percent.min(100.0))
                } else {
                    Color::Rgb(140, 140, 140)
                };
                for ch in cpu_val_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(cpu_color).bold());
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
                            .set_style(Style::default().fg(mem_color).bold());
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
                            .set_style(Style::default().fg(gpu_color).bold());
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
                            .set_style(Style::default().fg(vram_color).bold());
                        col += 1;
                    }
                }
                // USER
                for ch in user_val_str.chars() {
                    if col < heavy_inner.right() {
                        frame.buffer_mut()[(col, row_y)]
                            .set_char(ch)
                            .set_style(Style::default().fg(Color::Rgb(150, 150, 150)));
                        col += 1;
                    }
                }
            }
        }
    }

    // Right Column: GPU, Network, Disks, Storage
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
        ])
        .split(body_columns[1]);

    // GPU Card
    let gpu_block = Block::default()
        .title(" GPU & VRAM ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let gpu_inner = gpu_block.inner(right_chunks[0]);
    frame.render_widget(gpu_block, right_chunks[0]);

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

        let gpu_pct_str = format!("{:.1}% (", gpu.utilization_pct);
        let gpu_name_str = format!(", {})", gpu.name);

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
                (&gpu_pct_str, Color::Rgb(220, 220, 220), true),
                (&gpu_freq_str, gpu_freq_color, true),
                (", ", Color::Rgb(170, 170, 170), false),
                (&gpu_temp_str, gpu_temp_color, true),
                (&gpu_name_str, Color::Rgb(170, 170, 170), false),
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

    // Network Throughput Card
    let net_block = Block::default()
        .title(" Network Throughput ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let net_inner = net_block.inner(right_chunks[1]);
    frame.render_widget(net_block, right_chunks[1]);

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

    // Disk Throughput Card
    let disk_block = Block::default()
        .title(" Disk Throughput ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let disk_inner = disk_block.inner(right_chunks[2]);
    frame.render_widget(disk_block, right_chunks[2]);

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

    // Mounted Filesystems Card
    let storage_block = Block::default()
        .title(" Storage & Partitions ".fg(Color::Rgb(170, 170, 170)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let storage_inner = storage_block.inner(right_chunks[3]);
    frame.render_widget(storage_block, right_chunks[3]);

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
