#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

use std::{
    collections::HashMap,
    fs, io,
    time::{Duration, Instant},
};

use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style, Stylize},
    symbols,
    widgets::{Axis, Block, BorderType, Borders, Chart, Dataset, Gauge, GraphType, LineGauge, Tabs, Table, Row, Cell, TableState},
    text::Line,
};

#[derive(Default, Clone, Copy)]
struct CpuTicks {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuTicks {
    fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }
    fn idle_time(&self) -> u64 {
        self.idle + self.iowait
    }
}

fn read_cpu_ticks(buf: &mut String, cpus: &mut Vec<CpuTicks>) {
    cpus.clear();
    buf.clear();
    if let Ok(mut file) = fs::File::open("/proc/stat") {
        use std::io::Read;
        if file.read_to_string(buf).is_ok() {
            for line in buf.lines() {
                if line.starts_with("cpu") {
                    let mut parts = line.split_whitespace();
                    parts.next(); // skip "cpu..."
                    let user = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let nice = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let system = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let idle = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let iowait = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let irq = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let softirq = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let steal = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    cpus.push(CpuTicks {
                        user, nice, system, idle, iowait, irq, softirq, steal
                    });
                }
            }
        }
    }
}

fn calculate_usage(prev: &CpuTicks, curr: &CpuTicks) -> f64 {
    let prev_total = prev.total();
    let curr_total = curr.total();
    let total_diff = curr_total.saturating_sub(prev_total);
    if total_diff == 0 {
        return 0.0;
    }
    let prev_idle = prev.idle_time();
    let curr_idle = curr.idle_time();
    let idle_diff = curr_idle.saturating_sub(prev_idle);

    100.0 * (total_diff as f64 - idle_diff as f64) / total_diff as f64
}

fn read_memory(buf: &mut String) -> (u64, u64) {
    let mut total = 0;
    let mut available = 0;
    buf.clear();
    if let Ok(mut file) = fs::File::open("/proc/meminfo") {
        use std::io::Read;
        if file.read_to_string(buf).is_ok() {
            for line in buf.lines() {
                if line.starts_with("MemTotal:") {
                    total = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0);
                } else if line.starts_with("MemAvailable:") {
                    available = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0);
                }
            }
        }
    }
    // Convert KB to MB
    ((total / 1024), ((total.saturating_sub(available)) / 1024))
}
#[derive(Clone, Default)]
struct ProcessInfo {
    pid: u32,
    name: String,
    state: String,
    rss_kb: u64,
    utime: u64,
    stime: u64,
    read_bytes: u64,
    write_bytes: u64,
    threads: u32,
    uid: u32,
    user: String,
    cpu_percent: f64,
    read_speed: f64,
    write_speed: f64,
}

fn get_users() -> HashMap<u32, String> {
    let mut users = HashMap::new();
    if let Ok(passwd) = fs::read_to_string("/etc/passwd") {
        for line in passwd.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                if let Ok(uid) = parts[2].parse::<u32>() {
                    users.insert(uid, parts[0].to_string());
                }
            }
        }
    }
    users
}

fn format_bytes_dyn(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{:.0} B", bytes)
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes / 1024.0)
    } else if bytes < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", bytes / 1048576.0)
    } else if bytes < 1024.0 * 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", bytes / 1073741824.0)
    } else {
        format!("{:.1} TB", bytes / 1099511627776.0)
    }
}

fn read_processes(prev_procs: &mut HashMap<u32, ProcessInfo>, users: &HashMap<u32, String>, dt: f64) -> Vec<ProcessInfo> {
    let mut procs = Vec::new();
    let mut new_prev_procs = HashMap::new();
    
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.filter_map(Result::ok) {
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();
            if let Ok(pid) = file_name_str.parse::<u32>() {
                let mut p = ProcessInfo { pid, ..Default::default() };
                
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
                
                p.user = users.get(&p.uid).cloned().unwrap_or_else(|| p.uid.to_string());
                
                if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid)) {
                    if let Some(rparen) = stat.rfind(')') {
                        let rest = &stat[rparen + 1..];
                        let parts: Vec<&str> = rest.split_whitespace().collect();
                        if parts.len() >= 18 {
                            p.utime = parts[11].parse().unwrap_or(0);
                            p.stime = parts[12].parse().unwrap_or(0);
                            p.threads = parts[17].parse().unwrap_or(0);
                        }
                    }
                }
                
                if let Ok(io) = fs::read_to_string(format!("/proc/{}/io", pid)) {
                    for line in io.lines() {
                        if line.starts_with("read_bytes:") {
                            p.read_bytes = line.trim_start_matches("read_bytes:").trim().parse().unwrap_or(0);
                        } else if line.starts_with("write_bytes:") {
                            p.write_bytes = line.trim_start_matches("write_bytes:").trim().parse().unwrap_or(0);
                        }
                    }
                }
                
                if let Some(prev) = prev_procs.get(&pid) {
                    if dt > 0.0 {
                        let delta_ticks = (p.utime + p.stime).saturating_sub(prev.utime + prev.stime);
                        p.cpu_percent = (delta_ticks as f64) / dt;
                        p.read_speed = p.read_bytes.saturating_sub(prev.read_bytes) as f64 / dt;
                        p.write_speed = p.write_bytes.saturating_sub(prev.write_bytes) as f64 / dt;
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
    procs.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb));
    procs
}

fn get_color(usage: f64) -> Color {
    if usage < 50.0 {
        Color::Rgb(0, 255, 0)
    } else if usage < 85.0 {
        Color::Rgb(255, 255, 0)
    } else {
        Color::Rgb(255, 0, 0)
    }
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let tick_rate = Duration::from_secs(2);
    let mut last_tick = Instant::now();
    let mut cpu_history: Vec<(f64, f64)> = (0..100).map(|i| (i as f64, 0.0)).collect();
    let mut mem_history: Vec<(f64, f64)> = (0..100).map(|i| (i as f64, 0.0)).collect();

    let mut io_buf = String::with_capacity(8192);
    let mut prev_ticks = Vec::with_capacity(32);
    let mut curr_ticks = Vec::with_capacity(32);
    
    read_cpu_ticks(&mut io_buf, &mut prev_ticks);

    // Warmup delay to get initial CPU readings
    std::thread::sleep(Duration::from_millis(100));
    read_cpu_ticks(&mut io_buf, &mut curr_ticks);

    let mut global_usage = 0.0;
    let mut core_usages = Vec::new();
    let (mut total_mem, mut used_mem) = read_memory(&mut io_buf);

    let mut current_tab = 1;
    let mut prev_procs = HashMap::new();
    let users = get_users();
    let mut processes = read_processes(&mut prev_procs, &users, 0.0);
    let mut table_state = TableState::default();
    table_state.select(Some(0));

    if !prev_ticks.is_empty() && !curr_ticks.is_empty() {
        global_usage = calculate_usage(&prev_ticks[0], &curr_ticks[0]);
        for i in 1..curr_ticks.len().min(prev_ticks.len()) {
            core_usages.push(calculate_usage(&prev_ticks[i], &curr_ticks[i]));
        }
    }

    loop {

        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(area);

            let titles = vec![Line::from(" Processes (1) "), Line::from(" System (2) ")];
            let tabs = Tabs::new(titles)
                .style(Style::default().fg(Color::Rgb(255, 255, 255)))
                .select(current_tab)
                .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Rust System Monitor (btop-rs) - 'q' to quit "))
                .highlight_style(Style::default().fg(Color::Rgb(0, 255, 255)).bold())
                .divider("|");
            
            frame.render_widget(tabs, chunks[0]);

            if current_tab == 1 {
                let body_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(chunks[1]);

                let cpu_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(80), Constraint::Percentage(20)])
                    .split(body_chunks[0]);

                let dataset = Dataset::default()
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(get_color(global_usage)))
                    .data(&cpu_history);

                let chart = Chart::new(vec![dataset])
                    .block(Block::default().title("CPU History".fg(Color::Rgb(255, 255, 255))).borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(Color::Rgb(0, 100, 0))))
                    .x_axis(
                        Axis::default()
                            .bounds([0.0, 99.0])
                            .style(Style::default().fg(Color::Rgb(255, 255, 255)))
                            .labels(vec!["Older".bold().fg(Color::Rgb(255, 255, 255)), "Newer".bold().fg(Color::Rgb(255, 255, 255))]),
                    )
                    .y_axis(
                        Axis::default()
                            .bounds([0.0, 100.0])
                            .style(Style::default().fg(Color::Rgb(255, 255, 255)))
                            .labels(vec!["0%".bold().fg(Color::Rgb(255, 255, 255)), "50%".bold().fg(Color::Rgb(255, 255, 255)), "100%".bold().fg(Color::Rgb(255, 255, 255))]),
                    );
                frame.render_widget(chart, cpu_chunks[0]);

                let cores_block = Block::default()
                    .title("Cores".fg(Color::Rgb(255, 255, 255)))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(0, 100, 0)));
                let cores_inner = cores_block.inner(cpu_chunks[1]);
                frame.render_widget(cores_block, cpu_chunks[1]);

                let cores_layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(vec![Constraint::Ratio(1, core_usages.len().max(1) as u32); core_usages.len().max(1)])
                    .split(cores_inner);

                for (i, &usage) in core_usages.iter().enumerate() {
                    let usage_percent = usage.clamp(0.0, 100.0);
                    let core_gauge = LineGauge::default()
                        .block(Block::default())
                        .filled_style(Style::default().fg(get_color(usage)))
                        .style(Style::default().fg(Color::Rgb(80, 80, 80)))
                        .ratio(usage_percent / 100.0)
                        .label(Line::from(format!("C{:<2} {:>3.0}%", i, usage).fg(Color::Rgb(255, 255, 255))));
                    
                    if i < cores_layout.len() {
                        frame.render_widget(core_gauge, cores_layout[i]);
                    }
                }

                let mem_layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(0), Constraint::Length(3)])
                    .split(body_chunks[1]);

                let mem_percent_f64 = if total_mem > 0 { (used_mem as f64 / total_mem as f64) * 100.0 } else { 0.0 };
                let mem_percent = mem_percent_f64 as u16;

                let mem_dataset = Dataset::default()
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(get_color(mem_percent_f64)))
                    .data(&mem_history);

                let mem_chart = Chart::new(vec![mem_dataset])
                    .block(Block::default().title("Memory History".fg(Color::Rgb(255, 255, 255))).borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(Color::Rgb(150, 150, 0))))
                    .x_axis(
                        Axis::default()
                            .bounds([0.0, 99.0])
                            .style(Style::default().fg(Color::Rgb(255, 255, 255)))
                            .labels(vec!["Older".bold().fg(Color::Rgb(255, 255, 255)), "Newer".bold().fg(Color::Rgb(255, 255, 255))]),
                    )
                    .y_axis(
                        Axis::default()
                            .bounds([0.0, 100.0])
                            .style(Style::default().fg(Color::Rgb(255, 255, 255)))
                            .labels(vec!["0%".bold().fg(Color::Rgb(255, 255, 255)), "50%".bold().fg(Color::Rgb(255, 255, 255)), "100%".bold().fg(Color::Rgb(255, 255, 255))]),
                    );
                frame.render_widget(mem_chart, mem_layout[0]);

                let mem_gauge = Gauge::default()
                    .block(Block::default().title("Memory Usage".fg(Color::Rgb(255, 255, 255))).borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(Color::Rgb(150, 150, 0))))
                    .style(Style::default().fg(Color::Rgb(255, 255, 255)).bg(Color::Rgb(30, 30, 30)))
                    .gauge_style(Style::default().fg(get_color(mem_percent_f64)))
                    .percent(mem_percent.clamp(0, 100))
                    .label(format!("{} MB / {} MB", used_mem, total_mem).fg(Color::Rgb(255, 255, 255)));

                frame.render_widget(mem_gauge, mem_layout[1]);
            } else {
                let header_cells = ["PID", "User", "Name", "State", "Threads", "CPU %", "Mem", "IO Read/s", "IO Write/s"].iter().map(|h| Cell::from(*h).style(Style::default().fg(Color::Rgb(0, 255, 255))));
                let header = Row::new(header_cells).style(Style::default().bold()).height(1).bottom_margin(1);
                
                let rows = processes.iter().map(|p| {
                    let cells = vec![
                        Cell::from(p.pid.to_string()),
                        Cell::from(p.user.clone()),
                        Cell::from(p.name.clone()),
                        Cell::from(p.state.clone()),
                        Cell::from(p.threads.to_string()),
                        Cell::from(format!("{:.1}%", p.cpu_percent)),
                        Cell::from(format_bytes_dyn((p.rss_kb * 1024) as f64)),
                        Cell::from(format_bytes_dyn(p.read_speed)),
                        Cell::from(format_bytes_dyn(p.write_speed)),
                    ];
                    Row::new(cells).height(1)
                });
                
                let table = Table::new(rows, [
                    Constraint::Length(8),   // PID
                    Constraint::Length(12),  // User
                    Constraint::Min(20),     // Name
                    Constraint::Length(8),   // State
                    Constraint::Length(8),   // Threads
                    Constraint::Length(10),  // CPU %
                    Constraint::Length(10),  // Mem
                    Constraint::Length(12),  // IO Read/s
                    Constraint::Length(12),  // IO Write/s
                ])
                    .header(header)
                    .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Processes "))
                    .row_highlight_style(Style::default().bg(Color::Rgb(50, 50, 50)).fg(Color::Rgb(255, 255, 255)))
                    .style(Style::default().fg(Color::Rgb(255, 255, 255)));
                    
                frame.render_stateful_widget(table, chunks[1], &mut table_state);
            }
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')) {
                    break;
                } else if key.code == KeyCode::Tab || key.code == KeyCode::Right {
                    current_tab = (current_tab + 1) % 2;
                } else if key.code == KeyCode::BackTab || key.code == KeyCode::Left {
                    current_tab = (current_tab + 1) % 2;
                } else if key.code == KeyCode::Char('1') {
                    current_tab = 0;
                } else if key.code == KeyCode::Char('2') {
                    current_tab = 1;
                } else if key.code == KeyCode::Down {
                    let i = match table_state.selected() {
                        Some(i) => if i >= processes.len().saturating_sub(1) { i } else { i + 1 },
                        None => 0,
                    };
                    table_state.select(Some(i));
                } else if key.code == KeyCode::Up {
                    let i = match table_state.selected() {
                        Some(i) => if i == 0 { 0 } else { i - 1 },
                        None => 0,
                    };
                    table_state.select(Some(i));
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            processes = read_processes(&mut prev_procs, &users, last_tick.elapsed().as_secs_f64());
            std::mem::swap(&mut prev_ticks, &mut curr_ticks);
            read_cpu_ticks(&mut io_buf, &mut curr_ticks);
            let (tm, um) = read_memory(&mut io_buf);
            total_mem = tm;
            used_mem = um;
            let m_pct = if tm > 0 { (um as f64 / tm as f64) * 100.0 } else { 0.0 };

            if !prev_ticks.is_empty() && !curr_ticks.is_empty() {
                global_usage = calculate_usage(&prev_ticks[0], &curr_ticks[0]);
                core_usages.clear();
                for i in 1..curr_ticks.len().min(prev_ticks.len()) {
                    core_usages.push(calculate_usage(&prev_ticks[i], &curr_ticks[i]));
                }
            }

            for i in 0..99 {
                cpu_history[i].1 = cpu_history[i + 1].1;
                mem_history[i].1 = mem_history[i + 1].1;
            }
            cpu_history[99].1 = global_usage;
            mem_history[99].1 = m_pct;

            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    Ok(())
}
