//! System metrics and hardware telemetry collectors.
//!
//! Provides readers for `/proc/stat`, `/proc/meminfo`, `/proc/uptime`, `/proc/loadavg`,
//! `/sys/class/power_supply`, `/sys/class/drm`, `/sys/class/hwmon`, `/sys/class/thermal`,
//! `/sys/class/net`, and `/proc/diskstats`.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
};

use crate::utils::{format_bytes_dyn, parse_size_to_bytes};

/// Raw CPU accounting time ticks parsed from `/proc/stat`.
#[derive(Default, Clone, Copy)]
pub struct CpuTicks {
    /// Time spent in user mode.
    pub user: u64,
    /// Time spent in user mode with low priority (nice).
    pub nice: u64,
    /// Time spent in system (kernel) mode.
    pub system: u64,
    /// Time spent in the idle task.
    pub idle: u64,
    /// Time waiting for I/O to complete.
    pub iowait: u64,
    /// Time servicing hardware interrupts.
    pub irq: u64,
    /// Time servicing software interrupts.
    pub softirq: u64,
    /// Stolen time spent in other operating systems when in a virtualized environment.
    pub steal: u64,
}

impl CpuTicks {
    /// Calculates the sum of all CPU tick categories.
    ///
    /// # Returns
    /// Total cumulative ticks.
    pub fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    /// Calculates total idle time (idle + iowait).
    ///
    /// # Returns
    /// Cumulative idle ticks.
    pub fn idle_time(&self) -> u64 {
        self.idle + self.iowait
    }
}

/// Reads `/proc/stat` and populates a vector of `CpuTicks` for total and per-core CPU accounting.
///
/// # Arguments
/// * `buf` - Scratch string buffer to avoid heap allocations.
/// * `cpus` - Output vector populated with `CpuTicks` (index 0 is total, 1..N are per-core).
pub fn read_cpu_ticks(buf: &mut String, cpus: &mut Vec<CpuTicks>) {
    cpus.clear();
    buf.clear();
    if let Ok(mut file) = fs::File::open("/proc/stat")
        && file.read_to_string(buf).is_ok()
    {
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
                    user,
                    nice,
                    system,
                    idle,
                    iowait,
                    irq,
                    softirq,
                    steal,
                });
            }
        }
    }
}

/// Calculates CPU usage percentage between two consecutive `CpuTicks` sample points.
///
/// # Arguments
/// * `prev` - Earlier CPU ticks sample.
/// * `curr` - Current CPU ticks sample.
///
/// # Returns
/// Utilization percentage (0.0 to 100.0).
pub fn calculate_usage(prev: &CpuTicks, curr: &CpuTicks) -> f64 {
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

/// System memory and swap utilization metrics in megabytes.
#[derive(Clone, Copy, Default)]
pub struct MemoryMetrics {
    /// Total physical RAM available in megabytes.
    pub total_mem_mb: u64,
    /// Used physical RAM in megabytes.
    pub used_mem_mb: u64,
    /// Cached memory and buffers in megabytes.
    pub cached_mem_mb: u64,
    /// Total swap space in megabytes.
    pub total_swap_mb: u64,
    /// Used swap space in megabytes.
    pub used_swap_mb: u64,
}

/// Reads `/proc/meminfo` to calculate used/total physical memory, cached memory, and swap metrics.
///
/// # Arguments
/// * `buf` - Scratch buffer used to read `/proc/meminfo`.
///
/// # Returns
/// A populated `MemoryMetrics` structure.
pub fn read_memory(buf: &mut String) -> MemoryMetrics {
    let mut total = 0;
    let mut available = 0;
    let mut cached = 0;
    let mut buffers = 0;
    let mut sreclaimable = 0;
    let mut swap_total = 0;
    let mut swap_free = 0;
    buf.clear();
    if let Ok(mut file) = fs::File::open("/proc/meminfo")
        && file.read_to_string(buf).is_ok()
    {
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
            } else if line.starts_with("Cached:") {
                cached = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            } else if line.starts_with("Buffers:") {
                buffers = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            } else if line.starts_with("SReclaimable:") {
                sreclaimable = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            } else if line.starts_with("SwapTotal:") {
                swap_total = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            } else if line.starts_with("SwapFree:") {
                swap_free = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
            }
        }
    }
    MemoryMetrics {
        total_mem_mb: total / 1024,
        used_mem_mb: total.saturating_sub(available) / 1024,
        cached_mem_mb: (cached + buffers + sreclaimable) / 1024,
        total_swap_mb: swap_total / 1024,
        used_swap_mb: swap_total.saturating_sub(swap_free) / 1024,
    }
}

/// Power supply and battery status metrics.
#[derive(Clone, Default)]
pub struct BatteryInfo {
    /// Battery device identifier name (e.g. `BAT0`).
    pub name: String,
    /// Current power status (e.g. `Charging`, `Discharging`, `Full`, `Not charging`).
    pub status: String,
    /// Current battery capacity percentage (0 to 100).
    pub capacity_pct: u8,
    /// Current remaining energy in Watt-hours.
    pub energy_now_wh: Option<f64>,
    /// Full capacity energy in Watt-hours.
    pub energy_full_wh: Option<f64>,
    /// Live power draw or charging rate in Watts.
    pub power_w: Option<f64>,
    /// Reported battery health state (e.g. `Good`).
    pub health: String,
    /// Current power plan / profile (e.g. `Performance`, `Balanced`, `Power Saver`).
    pub power_plan: Option<String>,
}

/// Formats a raw Linux power profile or governor name into human-readable title case.
pub fn format_power_plan(raw: &str) -> String {
    let s = raw.trim().to_lowercase().replace('_', "-");
    match s.as_str() {
        "performance" => "Performance".to_string(),
        "balanced" => "Balanced".to_string(),
        "balanced-performance" | "balance-performance" => "Balanced Performance".to_string(),
        "balanced-power" | "balance-power" => "Balanced Power".to_string(),
        "power-saver" | "powersave" | "power" | "low-power" => "Power Saver".to_string(),
        "quiet" => "Quiet".to_string(),
        "cool" => "Cool".to_string(),
        "schedutil" => "Schedutil".to_string(),
        "ondemand" => "Ondemand".to_string(),
        "conservative" => "Conservative".to_string(),
        "userspace" => "Userspace".to_string(),
        _ if !s.is_empty() => s
            .split('-')
            .filter(|w| !w.is_empty())
            .map(|word| {
                let mut c = word.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Reads the current system power plan or platform profile.
///
/// Checks `/sys/firmware/acpi/platform_profile`, cpufreq energy performance preference,
/// and CPU scaling governor.
///
/// # Returns
/// `Some(String)` describing the active power plan, or `None` if undetectable.
pub fn read_power_plan() -> Option<String> {
    if let Ok(profile) = fs::read_to_string("/sys/firmware/acpi/platform_profile") {
        let trimmed = profile.trim();
        if !trimmed.is_empty() {
            let formatted = format_power_plan(trimmed);
            if !formatted.is_empty() {
                return Some(formatted);
            }
        }
    }

    if let Ok(epp) =
        fs::read_to_string("/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference")
            .or_else(|_| {
                fs::read_to_string(
                    "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference",
                )
            })
    {
        let trimmed = epp.trim();
        if !trimmed.is_empty() && trimmed != "default" {
            let formatted = format_power_plan(trimmed);
            if !formatted.is_empty() {
                return Some(formatted);
            }
        }
    }

    if let Ok(gov) = fs::read_to_string("/sys/devices/system/cpu/cpufreq/policy0/scaling_governor")
        .or_else(|_| fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"))
    {
        let trimmed = gov.trim();
        if !trimmed.is_empty() {
            let formatted = format_power_plan(trimmed);
            if !formatted.is_empty() {
                return Some(formatted);
            }
        }
    }

    None
}

/// Reads `/sys/class/power_supply` to query laptop battery state, capacity, energy, and wattage.
///
/// # Returns
/// `Some(BatteryInfo)` if a physical battery exists, or `None` on desktop systems.
pub fn read_battery() -> Option<BatteryInfo> {
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let p_type = fs::read_to_string(path.join("type"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            if p_type == "battery" {
                let name = entry.file_name().to_string_lossy().to_string();
                let status = fs::read_to_string(path.join("status"))
                    .unwrap_or_else(|_| "Unknown".to_string())
                    .trim()
                    .to_string();
                let capacity_pct = fs::read_to_string(path.join("capacity"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u8>().ok())
                    .unwrap_or(0);
                let health = fs::read_to_string(path.join("health"))
                    .unwrap_or_else(|_| "Good".to_string())
                    .trim()
                    .to_string();
                let voltage_u_v = fs::read_to_string(path.join("voltage_now"))
                    .or_else(|_| fs::read_to_string(path.join("voltage_min_design")))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok());

                let energy_now_wh = fs::read_to_string(path.join("energy_now"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|v| v / 1_000_000.0)
                    .or_else(|| {
                        let charge = fs::read_to_string(path.join("charge_now"))
                            .ok()
                            .and_then(|s| s.trim().parse::<f64>().ok())?;
                        let v = voltage_u_v?;
                        Some((charge * v) / 1_000_000_000_000.0)
                    });

                let energy_full_wh = fs::read_to_string(path.join("energy_full"))
                    .or_else(|_| fs::read_to_string(path.join("energy_full_design")))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|v| v / 1_000_000.0)
                    .or_else(|| {
                        let charge = fs::read_to_string(path.join("charge_full"))
                            .or_else(|_| fs::read_to_string(path.join("charge_full_design")))
                            .ok()
                            .and_then(|s| s.trim().parse::<f64>().ok())?;
                        let v = voltage_u_v?;
                        Some((charge * v) / 1_000_000_000_000.0)
                    });

                let power_w = fs::read_to_string(path.join("power_now"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|v| (v / 1_000_000.0).abs())
                    .or_else(|| {
                        let current = fs::read_to_string(path.join("current_now"))
                            .ok()
                            .and_then(|s| s.trim().parse::<f64>().ok())?;
                        let v = voltage_u_v?;
                        Some(((current * v) / 1_000_000_000_000.0).abs())
                    });

                let power_plan = read_power_plan();

                return Some(BatteryInfo {
                    name,
                    status,
                    capacity_pct,
                    energy_now_wh,
                    energy_full_wh,
                    power_w,
                    health,
                    power_plan,
                });
            }
        }
    }
    None
}

/// Information describing a connected visual monitor or display panel.
#[derive(Clone, Default)]
pub struct DisplayInfo {
    /// Monitor model descriptor parsed from EDID (tag 0xFC).
    pub name: String,
    /// Current video mode resolution (e.g. `2560x1440`).
    pub resolution: String,
    /// Physical screen diagonal length in inches.
    pub diagonal_inch: Option<u32>,
    /// Maximum vertical refresh rate in Hertz.
    pub refresh_rate_hz: Option<u32>,
    /// `true` for external displays (HDMI/DP), `false` for embedded laptop panels (eDP/LVDS).
    pub is_external: bool,
}

/// Reads `/sys/class/drm` and parses binary EDID data to discover connected monitors and specs.
///
/// # Returns
/// A list of `DisplayInfo` representing active connected outputs.
pub fn read_drm_displays() -> Vec<DisplayInfo> {
    let mut displays = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if let Ok(status) = fs::read_to_string(path.join("status"))
                && status.trim() == "connected"
            {
                let conn_name = entry.file_name().to_string_lossy().to_string();
                let is_external = !conn_name.contains("eDP")
                    && !conn_name.contains("LVDS")
                    && !conn_name.contains("DSI");
                let mut resolution = String::new();
                if let Ok(modes) = fs::read_to_string(path.join("modes"))
                    && let Some(first_mode) = modes.lines().next()
                {
                    resolution = first_mode.trim().to_string();
                }
                let mut name = String::new();
                let mut max_hz = None;
                let mut diagonal = None;

                if let Ok(edid) = fs::read(path.join("edid"))
                    && edid.len() >= 128
                {
                    let w_cm = edid[21] as f64;
                    let h_cm = edid[22] as f64;
                    if w_cm > 0.0 && h_cm > 0.0 {
                        let diag_in = ((w_cm * w_cm + h_cm * h_cm).sqrt() / 2.54).round() as u32;
                        diagonal = Some(diag_in);
                    }

                    for desc_offset in [54, 72, 90, 108] {
                        if desc_offset + 18 <= edid.len() {
                            let desc = &edid[desc_offset..desc_offset + 18];
                            if desc[0] == 0 && desc[1] == 0 && desc[2] == 0 {
                                if desc[3] == 0xFC {
                                    let s = String::from_utf8_lossy(&desc[5..18])
                                        .replace('\0', " ")
                                        .trim()
                                        .to_string();
                                    if !s.is_empty() {
                                        name = s;
                                    }
                                } else if desc[3] == 0xFD {
                                    let hz = desc[6] as u32;
                                    if hz > 0 {
                                        max_hz = Some(hz);
                                    }
                                }
                            }
                        }
                    }
                }

                if !resolution.is_empty() || !name.is_empty() {
                    displays.push(DisplayInfo {
                        name,
                        resolution,
                        diagonal_inch: diagonal,
                        refresh_rate_hz: max_hz,
                        is_external,
                    });
                }
            }
        }
    }
    displays
}

/// Determines the primary outbound local IPv4 address of the host machine.
///
/// # Returns
/// IP address string such as `"192.168.1.150"`.
pub fn get_local_ip() -> String {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
        && socket.connect("8.8.8.8:80").is_ok()
        && let Ok(addr) = socket.local_addr()
    {
        let ip = addr.ip();
        if !ip.is_loopback() && !ip.is_unspecified() {
            return ip.to_string();
        }
    }
    "127.0.0.1".to_string()
}

/// Detects the active Desktop Environment / Window Manager and session display server.
///
/// # Returns
/// Formatted string like `"KDE Plasma (Wayland)"` or `"i3 (X11)"`.
pub fn get_desktop_environment() -> String {
    let raw_de = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .or_else(|_| std::env::var("XDG_SESSION_DESKTOP"))
        .unwrap_or_default();

    let raw_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let type_formatted = match raw_type.to_lowercase().as_str() {
        "wayland" => "Wayland",
        "x11" => "X11",
        "tty" => "TTY",
        _ => "",
    };

    if !raw_de.is_empty() {
        if !type_formatted.is_empty() {
            format!("{} ({})", raw_de, type_formatted)
        } else {
            raw_de
        }
    } else if !type_formatted.is_empty() {
        type_formatted.to_string()
    } else {
        "Unknown".to_string()
    }
}

/// Aggregated system overview metadata and hardware identity.
#[derive(Clone, Default)]
pub struct SystemGeneralInfo {
    /// Host machine name.
    pub hostname: String,
    /// OS distribution name.
    pub os_name: String,
    /// Linux kernel release version.
    pub kernel: String,
    /// System uptime in seconds.
    pub uptime_secs: u64,
    /// Active Desktop Environment and session type.
    pub desktop: String,
    /// Current shell executable name.
    pub shell: String,
    /// Outbound local network IP address.
    pub local_ip: String,
    /// Primary network interface name.
    pub net_interface: String,
    /// Primary network interface link speed in Mbps.
    pub net_speed_mbps: u32,
    /// Primary network interface duplex mode (e.g. `Full`, `Half`).
    pub net_duplex: String,
    /// Active system locale code.
    pub locale: String,
    /// List of connected DRM displays.
    pub displays: Vec<DisplayInfo>,
}

/// Reads host system telemetry, OS version, kernel, uptime, load averages, and desktop environment.
///
/// # Returns
/// A populated `SystemGeneralInfo` structure.
pub fn read_system_general_info() -> SystemGeneralInfo {
    let hostname = fs::read_to_string("/etc/hostname")
        .or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname"))
        .unwrap_or_else(|_| "localhost".to_string())
        .trim()
        .to_string();

    let mut os_name = "Linux".to_string();
    if let Ok(os_release) = fs::read_to_string("/etc/os-release") {
        for line in os_release.lines() {
            if line.starts_with("PRETTY_NAME=") {
                os_name = line
                    .trim_start_matches("PRETTY_NAME=")
                    .trim_matches('"')
                    .to_string();
                break;
            }
        }
    }

    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .trim()
        .to_string();

    let uptime_secs = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|u| u.parse::<f64>().ok())
        })
        .map(|s| s.round() as u64)
        .unwrap_or(0);

    let desktop = get_desktop_environment();
    let shell = std::env::var("SHELL").unwrap_or_default();
    let shell_name = if let Some(pos) = shell.rfind('/') {
        shell[pos + 1..].to_string()
    } else if !shell.is_empty() {
        shell
    } else {
        "Unknown".to_string()
    };
    let local_ip = get_local_ip();

    let mut net_interface = String::new();
    let mut net_speed_mbps = 0;
    let mut net_duplex = String::new();
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.filter_map(Result::ok) {
            let ifname = entry.file_name().to_string_lossy().to_string();
            if ifname == "lo" {
                continue;
            }
            let operstate = fs::read_to_string(entry.path().join("operstate"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let is_up = operstate == "up";
            if is_up || net_interface.is_empty() {
                let speed = fs::read_to_string(entry.path().join("speed"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(0);
                let raw_duplex = fs::read_to_string(entry.path().join("duplex"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let duplex = match raw_duplex.to_lowercase().as_str() {
                    "full" => "Full".to_string(),
                    "half" => "Half".to_string(),
                    _ => "Unknown".to_string(),
                };
                net_interface = ifname;
                net_speed_mbps = speed;
                net_duplex = duplex;
                if is_up {
                    break;
                }
            }
        }
    }

    let locale = std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .unwrap_or_else(|_| "en_US.UTF-8".to_string());
    let displays = read_drm_displays();

    SystemGeneralInfo {
        hostname,
        os_name,
        kernel,
        uptime_secs,
        desktop,
        shell: shell_name,
        local_ip,
        net_interface,
        net_speed_mbps,
        net_duplex,
        locale,
        displays,
    }
}

/// Reads `/etc/passwd` to build a map of UID to human-readable username.
///
/// # Returns
/// A `HashMap<u32, String>` mapping UID to username.
pub fn get_users() -> HashMap<u32, String> {
    let mut users = HashMap::new();
    if let Ok(passwd) = fs::read_to_string("/etc/passwd") {
        for line in passwd.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3
                && let Ok(uid) = parts[2].parse::<u32>()
            {
                users.insert(uid, parts[0].to_string());
            }
        }
    }
    users
}

/// GPU core utilization, VRAM metrics, clocks, power, fans, and temperatures.
#[derive(Clone, Default)]
pub struct GpuMetrics {
    /// Human-readable GPU device model name.
    pub name: String,
    /// GPU driver name (e.g. "amdgpu", "i915", "nvidia").
    pub driver: String,
    /// VRAM memory vendor (e.g. "Samsung", "Micron", "Hynix").
    pub memory_vendor: String,
    /// PCIe Link generation and width (e.g. "PCIe 4.0 x8 (16.0 GT/s)").
    pub pcie_link: String,
    /// GPU core busy percentage (0.0 to 100.0).
    pub utilization_pct: f64,
    /// Allocated dedicated VRAM memory in megabytes.
    pub vram_used_mb: u64,
    /// Total available dedicated VRAM capacity in megabytes.
    pub vram_total_mb: u64,
    /// Shared system memory (GTT) allocated in megabytes.
    pub gtt_used_mb: u64,
    /// Total shared system memory (GTT) capacity in megabytes.
    pub gtt_total_mb: u64,
    /// GPU junction / hotspot temperature in degrees Celsius.
    pub temp_junction_c: u32,
    /// GPU edge / core temperature in degrees Celsius.
    pub temp_edge_c: u32,
    /// GPU VRAM / memory temperature in degrees Celsius.
    pub temp_mem_c: u32,
    /// GPU junction/core temperature in degrees Celsius.
    pub temp_c: u32,
    /// Current GPU core clock speed in MHz.
    pub cur_mhz: f64,
    /// Minimum base core clock in MHz.
    pub min_mhz: f64,
    /// Maximum boost core clock in MHz.
    pub max_mhz: f64,
    /// Memory clock frequency in MHz.
    pub mem_cur_mhz: f64,
    /// Current power consumption in Watts.
    pub power_w: f64,
    /// Power limit / cap in Watts.
    pub power_cap_w: f64,
    /// Core voltage in millivolts.
    pub voltage_mv: u32,
    /// Fan speed in RPM.
    pub fan_rpm: u32,
    /// Fan maximum speed in RPM.
    pub fan_max_rpm: u32,
    /// Fan duty percentage (0 to 100).
    pub fan_pct: u32,
}

/// Reads `/sys/class/drm` and `/sys/class/hwmon` to query AMD, NVIDIA, or Intel GPU metrics.
///
/// # Returns
/// A populated `GpuMetrics` structure.
pub fn read_gpu_metrics() -> GpuMetrics {
    let mut metrics = GpuMetrics {
        name: "GPU".to_string(),
        driver: String::new(),
        memory_vendor: String::new(),
        pcie_link: String::new(),
        utilization_pct: 0.0,
        vram_used_mb: 0,
        vram_total_mb: 0,
        gtt_used_mb: 0,
        gtt_total_mb: 0,
        temp_junction_c: 0,
        temp_edge_c: 0,
        temp_mem_c: 0,
        temp_c: 0,
        cur_mhz: 0.0,
        min_mhz: 0.0,
        max_mhz: 0.0,
        mem_cur_mhz: 0.0,
        power_w: 0.0,
        power_cap_w: 0.0,
        voltage_mv: 0,
        fan_rpm: 0,
        fan_max_rpm: 0,
        fan_pct: 0,
    };

    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for entry in entries.filter_map(Result::ok) {
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if name.starts_with("card") && !name.contains('-') {
                let dev_path = entry.path().join("device");
                if dev_path.exists() {
                    if let Ok(uevent) = fs::read_to_string(dev_path.join("uevent")) {
                        for line in uevent.lines() {
                            if let Some(drv) = line.strip_prefix("DRIVER=") {
                                metrics.driver = drv.to_string();
                                metrics.name = format!("GPU ({})", drv);
                            }
                        }
                    }

                    let mut vendor_id = String::new();
                    let mut device_id = String::new();
                    if let Ok(v) = fs::read_to_string(dev_path.join("vendor")) {
                        vendor_id = v.trim().to_lowercase();
                    }
                    if let Ok(d) = fs::read_to_string(dev_path.join("device")) {
                        device_id = d.trim().to_lowercase();
                    }

                    if vendor_id.contains("1002") {
                        if device_id.contains("7480") {
                            metrics.name = "AMD Radeon RX 7600".to_string();
                        } else {
                            metrics.name = "AMD Radeon Graphics".to_string();
                        }
                    } else if vendor_id.contains("10de") {
                        metrics.name = "NVIDIA GeForce GPU".to_string();
                    } else if vendor_id.contains("8086") {
                        metrics.name = "Intel Graphics".to_string();
                    }

                    if let Ok(busy_str) = fs::read_to_string(dev_path.join("gpu_busy_percent"))
                        && let Ok(busy) = busy_str.trim().parse::<f64>()
                    {
                        metrics.utilization_pct = busy;
                    }

                    if let Ok(total_str) = fs::read_to_string(dev_path.join("mem_info_vram_total"))
                        && let Ok(total_bytes) = total_str.trim().parse::<u64>()
                    {
                        metrics.vram_total_mb = total_bytes / (1024 * 1024);
                    }
                    if let Ok(used_str) = fs::read_to_string(dev_path.join("mem_info_vram_used"))
                        && let Ok(used_bytes) = used_str.trim().parse::<u64>()
                    {
                        metrics.vram_used_mb = used_bytes / (1024 * 1024);
                    }

                    if let Ok(gtt_tot) = fs::read_to_string(dev_path.join("mem_info_gtt_total"))
                        && let Ok(tot_bytes) = gtt_tot.trim().parse::<u64>()
                    {
                        metrics.gtt_total_mb = tot_bytes / (1024 * 1024);
                    }
                    if let Ok(gtt_u) = fs::read_to_string(dev_path.join("mem_info_gtt_used"))
                        && let Ok(u_bytes) = gtt_u.trim().parse::<u64>()
                    {
                        metrics.gtt_used_mb = u_bytes / (1024 * 1024);
                    }

                    if let Ok(vendor_str) =
                        fs::read_to_string(dev_path.join("mem_info_vram_vendor"))
                    {
                        let v = vendor_str.trim();
                        if !v.is_empty() {
                            let mut chars = v.chars();
                            if let Some(first) = chars.next() {
                                metrics.memory_vendor =
                                    format!("{}{}", first.to_uppercase(), chars.as_str());
                            }
                        }
                    }

                    let cur_speed = fs::read_to_string(dev_path.join("current_link_speed"))
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    let cur_width = fs::read_to_string(dev_path.join("current_link_width"))
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    if !cur_speed.is_empty() && !cur_width.is_empty() {
                        metrics.pcie_link = format!("{} x{}", cur_speed, cur_width);
                    } else if !cur_speed.is_empty() {
                        metrics.pcie_link = cur_speed;
                    }

                    if let Ok(pp_sclk) = fs::read_to_string(dev_path.join("pp_dpm_sclk")) {
                        let mut first_mhz = None;
                        let mut last_mhz = None;
                        for line in pp_sclk.lines() {
                            let mut parts = line.split_whitespace();
                            if let Some(_idx) = parts.next()
                                && let Some(freq_str) = parts.next()
                            {
                                let num_str: String =
                                    freq_str.chars().filter(|c| c.is_ascii_digit()).collect();
                                if let Ok(freq) = num_str.parse::<f64>() {
                                    if first_mhz.is_none() {
                                        first_mhz = Some(freq);
                                    }
                                    last_mhz = Some(freq);
                                    if line.contains('*') {
                                        metrics.cur_mhz = freq;
                                    }
                                }
                            }
                        }
                        if let Some(f) = first_mhz {
                            metrics.min_mhz = f;
                        }
                        if let Some(l) = last_mhz {
                            metrics.max_mhz = l;
                        }
                    }

                    if let Ok(hwmon_entries) = fs::read_dir(dev_path.join("hwmon")) {
                        for h_entry in hwmon_entries.filter_map(Result::ok) {
                            let h_path = h_entry.path();

                            // Temperatures
                            for i in 1..=4 {
                                if let Ok(input_str) =
                                    fs::read_to_string(h_path.join(format!("temp{}_input", i)))
                                    && let Ok(temp_milli) = input_str.trim().parse::<u32>()
                                {
                                    let temp_val = temp_milli / 1000;
                                    let label =
                                        fs::read_to_string(h_path.join(format!("temp{}_label", i)))
                                            .map(|s| s.trim().to_lowercase())
                                            .unwrap_or_default();
                                    if label == "edge" || (i == 1 && metrics.temp_edge_c == 0) {
                                        metrics.temp_edge_c = temp_val;
                                    } else if label == "junction"
                                        || label == "hotspot"
                                        || (i == 2 && metrics.temp_junction_c == 0)
                                    {
                                        metrics.temp_junction_c = temp_val;
                                    } else if label == "mem"
                                        || label == "vram"
                                        || (i == 3 && metrics.temp_mem_c == 0)
                                    {
                                        metrics.temp_mem_c = temp_val;
                                    }
                                }
                            }

                            // Power
                            if let Ok(p_str) = fs::read_to_string(h_path.join("power1_average"))
                                .or_else(|_| fs::read_to_string(h_path.join("power1_input")))
                                && let Ok(p_micro) = p_str.trim().parse::<f64>()
                            {
                                metrics.power_w = p_micro / 1_000_000.0;
                            }
                            if let Ok(cap_str) = fs::read_to_string(h_path.join("power1_cap"))
                                && let Ok(cap_micro) = cap_str.trim().parse::<f64>()
                            {
                                metrics.power_cap_w = cap_micro / 1_000_000.0;
                            }

                            // Voltage
                            if let Ok(v_str) = fs::read_to_string(h_path.join("in0_input"))
                                && let Ok(v_milli) = v_str.trim().parse::<u32>()
                            {
                                metrics.voltage_mv = v_milli;
                            }

                            // Fan speed & duty
                            if let Ok(fan_str) = fs::read_to_string(h_path.join("fan1_input"))
                                && let Ok(rpm) = fan_str.trim().parse::<u32>()
                            {
                                metrics.fan_rpm = rpm;
                            }
                            if let Ok(fan_max_str) = fs::read_to_string(h_path.join("fan1_max"))
                                && let Ok(max_rpm) = fan_max_str.trim().parse::<u32>()
                            {
                                metrics.fan_max_rpm = max_rpm;
                            }
                            if let Ok(pwm_str) = fs::read_to_string(h_path.join("pwm1"))
                                && let Ok(pwm) = pwm_str.trim().parse::<u32>()
                            {
                                let pwm_max = fs::read_to_string(h_path.join("pwm1_max"))
                                    .ok()
                                    .and_then(|s| s.trim().parse::<u32>().ok())
                                    .unwrap_or(255);
                                if let Some(pct) = (pwm * 100).checked_div(pwm_max) {
                                    metrics.fan_pct = pct;
                                }
                            }

                            // Frequencies
                            if metrics.cur_mhz <= 0.0
                                && let Ok(freq_str) = fs::read_to_string(h_path.join("freq1_input"))
                                && let Ok(freq_hz) = freq_str.trim().parse::<f64>()
                            {
                                metrics.cur_mhz = freq_hz / 1_000_000.0;
                            }
                            if let Ok(mclk_str) = fs::read_to_string(h_path.join("freq2_input"))
                                && let Ok(mclk_hz) = mclk_str.trim().parse::<f64>()
                            {
                                metrics.mem_cur_mhz = mclk_hz / 1_000_000.0;
                            }
                        }
                    }

                    if metrics.temp_edge_c > 0 {
                        metrics.temp_c = metrics.temp_edge_c;
                    } else if metrics.temp_junction_c > 0 {
                        metrics.temp_c = metrics.temp_junction_c;
                    }

                    if metrics.min_mhz <= 0.0 {
                        metrics.min_mhz = 200.0;
                    }
                    if metrics.max_mhz <= metrics.min_mhz {
                        metrics.max_mhz = 2500.0;
                    }

                    if metrics.vram_total_mb > 0 || metrics.utilization_pct > 0.0 {
                        return metrics;
                    }
                }
            }
        }
    }

    metrics
}

/// Reads the current average, minimum base, and maximum boost CPU frequencies across all online cores.
///
/// # Returns
/// A tuple `(current_mhz, min_mhz, max_mhz)`.
pub fn read_cpu_freq_info() -> (f64, f64, f64) {
    let mut cur_sum = 0.0;
    let mut count = 0.0;

    if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
        for entry in entries.filter_map(Result::ok) {
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if name.starts_with("cpu")
                && name[3..].chars().all(|c| c.is_ascii_digit())
                && let Ok(cur_str) =
                    fs::read_to_string(entry.path().join("cpufreq/scaling_cur_freq"))
                && let Ok(cur_khz) = cur_str.trim().parse::<f64>()
            {
                cur_sum += cur_khz / 1000.0;
                count += 1.0;
            }
        }
    }

    if count == 0.0
        && let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo")
    {
        for line in cpuinfo.lines() {
            if line.starts_with("cpu MHz")
                && let Some(pos) = line.find(':')
                && let Ok(mhz) = line[pos + 1..].trim().parse::<f64>()
            {
                cur_sum += mhz;
                count += 1.0;
            }
        }
    }

    let cur_mhz = if count > 0.0 { cur_sum / count } else { 0.0 };

    let min_mhz = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq")
        .or_else(|_| fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_min_freq"))
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|khz| khz / 1000.0)
        .unwrap_or(800.0);

    let max_mhz = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .or_else(|_| fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq"))
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|khz| khz / 1000.0)
        .unwrap_or(4500.0);

    (cur_mhz, min_mhz, max_mhz)
}

/// Reads the package/die CPU temperature from `/sys/class/hwmon` or `/sys/class/thermal`.
///
/// # Returns
/// Temperature in degrees Celsius (or 0 if unavailable).
pub fn read_cpu_temp() -> u32 {
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = fs::read_to_string(path.join("name"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            if name.contains("k10temp")
                || name.contains("coretemp")
                || name.contains("zenpower")
                || name.contains("cpu")
                || name.contains("k8temp")
            {
                for temp_file in &["temp1_input", "temp2_input", "temp3_input"] {
                    if let Ok(temp_str) = fs::read_to_string(path.join(temp_file))
                        && let Ok(milli) = temp_str.trim().parse::<u32>()
                        && milli > 0
                        && milli < 150_000
                    {
                        return milli / 1000;
                    }
                }
            }
        }
    }

    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let t_type = fs::read_to_string(path.join("type"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            if (t_type.contains("pkg")
                || t_type.contains("cpu")
                || t_type.contains("x86")
                || t_type.contains("acpi"))
                && let Ok(temp_str) = fs::read_to_string(path.join("temp"))
                && let Ok(milli) = temp_str.trim().parse::<u32>()
                && milli > 0
                && milli < 150_000
            {
                return milli / 1000;
            }
        }
    }

    0
}

/// Reads the system RAM / DIMM temperature in degrees Celsius from `/sys/class/hwmon` or `/sys/class/thermal`.
/// Returns `None` if no hardware DIMM/RAM thermal sensor is detected.
pub fn read_ram_temp() -> Option<u32> {
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = fs::read_to_string(path.join("name"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();

            // Direct RAM temperature drivers
            let is_ram_driver = name.contains("jc42")
                || name.contains("spd5118")
                || name.contains("i5500")
                || name.contains("i5k_amb")
                || name.contains("sbrmi")
                || (name.contains("dram") && !name.contains("gpu"))
                || (name.contains("dimm") && !name.contains("gpu"));

            if is_ram_driver && let Ok(h_entries) = fs::read_dir(&path) {
                for h_entry in h_entries.filter_map(Result::ok) {
                    let fname = h_entry.file_name().to_string_lossy().to_string();
                    if fname.starts_with("temp")
                        && fname.ends_with("_input")
                        && let Ok(temp_str) = fs::read_to_string(h_entry.path())
                        && let Ok(milli) = temp_str.trim().parse::<u32>()
                        && milli > 0
                        && milli < 130_000
                    {
                        return Some(milli / 1000);
                    }
                }
            }

            // Exclude GPU / NVMe drivers from generic labeling check
            let is_non_gpu = !name.contains("amdgpu")
                && !name.contains("nouveau")
                && !name.contains("nvidia")
                && !name.contains("i915")
                && !name.contains("xe")
                && !name.contains("nvme");

            if is_non_gpu && let Ok(h_entries) = fs::read_dir(&path) {
                for h_entry in h_entries.filter_map(Result::ok) {
                    let fname = h_entry.file_name().to_string_lossy().to_string();
                    if fname.starts_with("temp") && fname.ends_with("_label") {
                        let label = fs::read_to_string(h_entry.path())
                            .unwrap_or_default()
                            .trim()
                            .to_lowercase();
                        if (label.contains("dimm")
                            || label.contains("dram")
                            || label.contains("sodimm")
                            || label.contains("ddr")
                            || label.contains("tmem")
                            || label.contains("tdimm")
                            || label == "memory")
                            && let input_name = fname.replace("_label", "_input")
                            && let Ok(temp_str) = fs::read_to_string(path.join(input_name))
                            && let Ok(milli) = temp_str.trim().parse::<u32>()
                            && milli > 0
                            && milli < 130_000
                        {
                            return Some(milli / 1000);
                        }
                    }
                }
            }
        }
    }

    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let t_type = fs::read_to_string(path.join("type"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            if !t_type.contains("gpu")
                && !t_type.contains("amdgpu")
                && !t_type.contains("nvme")
                && (t_type.contains("dimm")
                    || t_type.contains("dram")
                    || t_type.contains("sodimm")
                    || t_type.contains("ddr")
                    || t_type.contains("tmem")
                    || t_type.contains("memory")
                    || t_type.contains("ram"))
                && let Ok(temp_str) = fs::read_to_string(path.join("temp"))
                && let Ok(milli) = temp_str.trim().parse::<u32>()
                && milli > 0
                && milli < 130_000
            {
                return Some(milli / 1000);
            }
        }
    }

    None
}

/// Reads `/proc/cpuinfo` to extract the official marketing model name of the CPU.
///
/// # Returns
/// Model string such as `"AMD Ryzen 5 3600X 6-Core Processor"`.
pub fn get_cpu_model() -> String {
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        for line in cpuinfo.lines() {
            if line.starts_with("model name")
                && let Some(pos) = line.find(':')
            {
                return line[pos + 1..].trim().to_string();
            }
        }
    }
    "CPU".to_string()
}

/// Queries hardware DMI table 17 via `dmidecode` or uses CPU architecture heuristics
/// to determine RAM generation (DDR4/DDR5) and rated clock speed in MHz.
///
/// # Arguments
/// * `total_mb` - Total RAM in megabytes.
/// * `cpu_model` - Processor model string used for heuristic fallback.
///
/// # Returns
/// Formatted string like `"16GB DDR4@3200MHz"` or `"32GB DDR5@5600MHz"`.
/// Reads SMBIOS Type 17 memory device information directly from sysfs if accessible.
fn read_dmi_sysfs() -> Option<(String, String)> {
    if let Ok(entries) = fs::read_dir("/sys/firmware/dmi/entries") {
        for entry in entries.filter_map(Result::ok) {
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if name.starts_with("17-") {
                let raw_path = entry.path().join("raw");
                if let Ok(data) = fs::read(raw_path)
                    && data.len() >= 0x16
                {
                    let mem_type_byte = data[0x12];
                    let speed = if data.len() >= 0x22 {
                        let configured_speed = u16::from_le_bytes([data[0x20], data[0x21]]);
                        if configured_speed > 0 && configured_speed != 0xFFFF {
                            configured_speed
                        } else {
                            u16::from_le_bytes([data[0x15], data[0x16]])
                        }
                    } else {
                        u16::from_le_bytes([data[0x15], data[0x16]])
                    };

                    let ram_type = match mem_type_byte {
                        0x12 => "DDR",
                        0x13 => "DDR2",
                        0x18 => "DDR3",
                        0x1A => "DDR4",
                        0x1B => "LPDDR",
                        0x1C => "LPDDR2",
                        0x1D => "LPDDR3",
                        0x1E => "LPDDR4",
                        0x22 => "DDR5",
                        0x23 => "LPDDR5",
                        _ => "",
                    };

                    if !ram_type.is_empty() {
                        let speed_str = if speed > 0 && speed != 0xFFFF {
                            format!("{}MHz", speed)
                        } else {
                            String::new()
                        };
                        return Some((ram_type.to_string(), speed_str));
                    }
                }
            }
        }
    }
    None
}

/// Heuristically determines the RAM type and standard clock frequency from processor architecture and SKU.
/// Returns `None` if no confident architectural inference can be made without hardware DMI privileges.
pub fn detect_ram_by_cpu(cpu_model: &str) -> Option<(&'static str, &'static str)> {
    let cpu = cpu_model.to_lowercase();

    // Apple Silicon
    if cpu.contains("apple") {
        if cpu.contains("m1 pro")
            || cpu.contains("m1 max")
            || cpu.contains("m1 ultra")
            || cpu.contains("m2")
            || cpu.contains("m3")
            || cpu.contains("m4")
        {
            return Some(("LPDDR5", "6400MHz"));
        }
        if cpu.contains("m1") {
            return Some(("LPDDR4X", "4266MHz"));
        }
    }

    // AMD Processors
    if cpu.contains("ryzen") || cpu.contains("epyc") || cpu.contains("threadripper") {
        // Ryzen AI 300 (Strix Point / Zen 5 Mobile)
        if cpu.contains("ryzen ai")
            || cpu.contains("ai 9")
            || cpu.contains("ai 7")
            || cpu.contains("ai 5")
        {
            return Some(("DDR5", "5600MHz"));
        }

        // Zen 5 (9000 series Desktop)
        if cpu.contains("9000")
            || cpu.contains("9950")
            || cpu.contains("9900")
            || cpu.contains("9800")
            || cpu.contains("9700")
            || cpu.contains("9600")
        {
            return Some(("DDR5", "5600MHz"));
        }

        // Zen 4 / Hawk Point / Phoenix (8000 series Mobile & Desktop)
        if cpu.contains("8000")
            || cpu.contains("8945")
            || cpu.contains("8845")
            || cpu.contains("8645")
            || cpu.contains("8545")
            || cpu.contains("8840")
            || cpu.contains("8640")
            || cpu.contains("8540")
            || cpu.contains("8440")
            || cpu.contains("8700")
            || cpu.contains("8600")
            || cpu.contains("8500")
            || cpu.contains("8400")
            || cpu.contains("8300")
        {
            return Some(("DDR5", "5600MHz"));
        }

        // Zen 4 / Dragon Range / Phoenix / Rembrandt-R (7000 series)
        // 7030 series (Barcelo-R, Zen 3, DDR4-3200)
        if cpu.contains("7730") || cpu.contains("7530") || cpu.contains("7330") {
            return Some(("DDR4", "3200MHz"));
        }
        // 7035 series (Rembrandt-R, Zen 3+, DDR5-4800)
        if cpu.contains("7735")
            || cpu.contains("7535")
            || cpu.contains("7435")
            || cpu.contains("7335")
        {
            return Some(("DDR5", "4800MHz"));
        }
        // 7020 series (Mendocino, LPDDR5-5500)
        if cpu.contains("7520") || cpu.contains("7320") || cpu.contains("7220") {
            return Some(("LPDDR5", "5500MHz"));
        }
        // 7040 / 7045 series (Phoenix / Dragon Range Zen 4) and 7000 series AM5 desktop
        if cpu.contains("7000")
            || cpu.contains("7950")
            || cpu.contains("7945")
            || cpu.contains("7940")
            || cpu.contains("7900")
            || cpu.contains("7845")
            || cpu.contains("7840")
            || cpu.contains("7800")
            || cpu.contains("7745")
            || cpu.contains("7700")
            || cpu.contains("7645")
            || cpu.contains("7640")
            || cpu.contains("7600")
            || cpu.contains("7540")
            || cpu.contains("7500")
            || cpu.contains("7440")
            || cpu.contains("79")
            || cpu.contains("78")
            || cpu.contains("77")
            || cpu.contains("76")
        {
            return Some(("DDR5", "5600MHz"));
        }

        // Zen 3+ (6000 series Mobile: 6980, 6900, 6850, 6800, 6600, 6500) - DDR5 only platform
        if cpu.contains("6000")
            || cpu.contains("6980")
            || cpu.contains("6900")
            || cpu.contains("6850")
            || cpu.contains("6800")
            || cpu.contains("6600")
            || cpu.contains("6500")
        {
            return Some(("DDR5", "4800MHz"));
        }

        // Threadripper 7000 / EPYC 9004/9005
        if (cpu.contains("threadripper") || cpu.contains("epyc"))
            && (cpu.contains("7") || cpu.contains("9"))
        {
            return Some(("DDR5", "5600MHz"));
        }

        // Zen 1 / Zen+ / Zen 2 / Zen 3 (1000..5000 series)
        if cpu.contains("5000")
            || cpu.contains("4000")
            || cpu.contains("3000")
            || cpu.contains("2000")
            || cpu.contains("1000")
            || cpu.contains("5950")
            || cpu.contains("5900")
            || cpu.contains("5800")
            || cpu.contains("5700")
            || cpu.contains("5600")
            || cpu.contains("5500")
            || cpu.contains("4800")
            || cpu.contains("4700")
            || cpu.contains("4600")
            || cpu.contains("4500")
            || cpu.contains("3950")
            || cpu.contains("3900")
            || cpu.contains("3800")
            || cpu.contains("3700")
            || cpu.contains("3600")
            || cpu.contains("3500")
            || cpu.contains("3400")
            || cpu.contains("3300")
            || cpu.contains("3200")
            || cpu.contains("3100")
            || cpu.contains("2700")
            || cpu.contains("2600")
            || cpu.contains("2500")
            || cpu.contains("2400")
            || cpu.contains("2200")
            || cpu.contains("1800")
            || cpu.contains("1700")
            || cpu.contains("1600")
            || cpu.contains("1500")
            || cpu.contains("1400")
            || cpu.contains("1200")
            || cpu.contains("ryzen")
        {
            return Some(("DDR4", "3200MHz"));
        }
    }

    if cpu.contains("fx-") || cpu.contains("phenom") || cpu.contains("athlon ii") {
        return Some(("DDR3", "1600MHz"));
    }

    // Intel Processors
    if cpu.contains("intel")
        || cpu.contains("core")
        || cpu.contains("xeon")
        || cpu.contains("celeron")
        || cpu.contains("pentium")
    {
        // Core Ultra Series 1 & 2 (Meteor Lake, Lunar Lake, Arrow Lake)
        if cpu.contains("ultra") {
            return Some(("DDR5", "5600MHz"));
        }

        // Core 100/200 series (e.g. Core 7 150U, Core 5 120U)
        if cpu.contains("150u") || cpu.contains("120u") || cpu.contains("100u") {
            return Some(("DDR5", "5200MHz"));
        }

        // 14th Gen (Raptor Lake Refresh)
        if cpu.contains("14th")
            || cpu.contains("i9-14")
            || cpu.contains("i7-14")
            || cpu.contains("i5-14")
            || cpu.contains("i3-14")
            || cpu.contains("14900")
            || cpu.contains("14700")
            || cpu.contains("14650")
            || cpu.contains("14600")
            || cpu.contains("14500")
            || cpu.contains("14450")
            || cpu.contains("14400")
            || cpu.contains("14100")
        {
            return Some(("DDR5", "5600MHz"));
        }

        // 13th Gen (Raptor Lake)
        if cpu.contains("13th")
            || cpu.contains("i9-13")
            || cpu.contains("i7-13")
            || cpu.contains("i5-13")
            || cpu.contains("i3-13")
            || cpu.contains("13980")
            || cpu.contains("13950")
            || cpu.contains("13900")
            || cpu.contains("13850")
            || cpu.contains("13700")
            || cpu.contains("13650")
            || cpu.contains("13620")
            || cpu.contains("13600")
            || cpu.contains("13500")
            || cpu.contains("13450")
            || cpu.contains("13420")
            || cpu.contains("13400")
            || cpu.contains("13100")
        {
            return Some(("DDR5", "5600MHz"));
        }

        // 12th Gen (Alder Lake)
        if cpu.contains("12th")
            || cpu.contains("i9-12")
            || cpu.contains("i7-12")
            || cpu.contains("i5-12")
            || cpu.contains("i3-12")
            || cpu.contains("12950")
            || cpu.contains("12900")
            || cpu.contains("12850")
            || cpu.contains("12800")
            || cpu.contains("12700")
            || cpu.contains("12650")
            || cpu.contains("12600")
            || cpu.contains("12500")
            || cpu.contains("12450")
            || cpu.contains("12400")
            || cpu.contains("12100")
        {
            if cpu.contains("h") || cpu.contains("hx") || cpu.contains("hk") {
                return Some(("DDR5", "4800MHz"));
            }
            return Some(("DDR4", "3200MHz"));
        }

        // 6th through 11th Gen
        if cpu.contains("11th")
            || cpu.contains("10th")
            || cpu.contains("9th")
            || cpu.contains("8th")
            || cpu.contains("7th")
            || cpu.contains("6th")
            || cpu.contains("i9-11")
            || cpu.contains("i9-10")
            || cpu.contains("i9-9")
            || cpu.contains("i7-11")
            || cpu.contains("i7-10")
            || cpu.contains("i7-9")
            || cpu.contains("i7-8")
            || cpu.contains("i7-7")
            || cpu.contains("i7-6")
            || cpu.contains("i5-11")
            || cpu.contains("i5-10")
            || cpu.contains("i5-9")
            || cpu.contains("i5-8")
            || cpu.contains("i5-7")
            || cpu.contains("i5-6")
            || cpu.contains("i3-11")
            || cpu.contains("i3-10")
            || cpu.contains("i3-9")
            || cpu.contains("i3-8")
            || cpu.contains("i3-7")
            || cpu.contains("i3-6")
            || cpu.contains("11800")
            || cpu.contains("11700")
            || cpu.contains("11600")
            || cpu.contains("11400")
            || cpu.contains("10750")
            || cpu.contains("10700")
            || cpu.contains("10600")
            || cpu.contains("10400")
            || cpu.contains("9750")
            || cpu.contains("9700")
            || cpu.contains("9400")
            || cpu.contains("8750")
            || cpu.contains("8700")
            || cpu.contains("8400")
            || cpu.contains("7700")
            || cpu.contains("7500")
            || cpu.contains("6700")
            || cpu.contains("6500")
        {
            return Some(("DDR4", "3200MHz"));
        }

        // 2nd through 5th Gen (Sandy Bridge, Ivy Bridge, Haswell, Broadwell)
        if cpu.contains("2nd")
            || cpu.contains("3rd")
            || cpu.contains("4th")
            || cpu.contains("5th")
            || cpu.contains("i7-2")
            || cpu.contains("i7-3")
            || cpu.contains("i7-4")
            || cpu.contains("i7-5")
            || cpu.contains("i5-2")
            || cpu.contains("i5-3")
            || cpu.contains("i5-4")
            || cpu.contains("i5-5")
            || cpu.contains("i3-2")
            || cpu.contains("i3-3")
            || cpu.contains("i3-4")
            || cpu.contains("i3-5")
            || cpu.contains("4770")
            || cpu.contains("3770")
            || cpu.contains("2600")
            || cpu.contains("2500")
        {
            return Some(("DDR3", "1600MHz"));
        }

        if cpu.contains("core 2")
            || cpu.contains("core2")
            || cpu.contains("q6600")
            || cpu.contains("e8400")
        {
            return Some(("DDR2", "800MHz"));
        }

        if cpu.contains("pentium") || cpu.contains("celeron") {
            return Some(("DDR3", "1333MHz"));
        }
    }

    None
}

/// Queries hardware DMI table 17 via `dmidecode` or uses CPU architecture heuristics
/// to determine RAM generation (DDR4/DDR5) and rated clock speed in MHz.
///
/// # Arguments
/// * `total_mb` - Total RAM in megabytes.
/// * `cpu_model` - Processor model string used for heuristic fallback.
///
/// # Returns
/// Formatted string like `"16GB DDR4@3200MHz"` or `"32GB DDR5@5600MHz"`, or `"16GB [root permissions required]"`.
pub fn get_ram_info(total_mb: u64, cpu_model: &str) -> String {
    let total_gb = ((total_mb as f64) / 1024.0).round() as u64;

    // 1. Try reading sysfs DMI raw entries if accessible
    if let Some((ram_type, speed)) = read_dmi_sysfs() {
        if !speed.is_empty() {
            return format!("{}GB {}@{}", total_gb, ram_type.to_uppercase(), speed);
        } else {
            return format!("{}GB {}", total_gb, ram_type.to_uppercase());
        }
    }

    // 2. Try dmidecode (direct or passwordless sudo)
    for bin in &["dmidecode", "/usr/sbin/dmidecode", "/sbin/dmidecode"] {
        let output = std::process::Command::new(bin)
            .args(["-t", "17"])
            .output()
            .or_else(|_| {
                std::process::Command::new("sudo")
                    .args(["-n", bin, "-t", "17"])
                    .output()
            });

        if let Ok(output) = output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut ram_type = String::new();
            let mut speed = String::new();

            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Type:")
                    && !trimmed.contains("Error")
                    && !trimmed.contains("Unknown")
                {
                    let t = trimmed.trim_start_matches("Type:").trim();
                    if t.starts_with("DDR") || t.starts_with("LPDDR") {
                        ram_type = t.to_string();
                    }
                }
                if (trimmed.starts_with("Speed:")
                    || trimmed.starts_with("Configured Memory Speed:"))
                    && !trimmed.contains("Unknown")
                {
                    let s = if trimmed.starts_with("Configured Memory Speed:") {
                        trimmed
                            .trim_start_matches("Configured Memory Speed:")
                            .trim()
                    } else {
                        trimmed.trim_start_matches("Speed:").trim()
                    };
                    let s_num: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
                    if !s_num.is_empty() {
                        speed = format!("{}MHz", s_num);
                    }
                }
            }

            if !ram_type.is_empty() {
                if !speed.is_empty() {
                    return format!("{}GB {}@{}", total_gb, ram_type.to_uppercase(), speed);
                } else {
                    return format!("{}GB {}", total_gb, ram_type.to_uppercase());
                }
            }
        }
    }

    // 3. Fallback to CPU architecture heuristic if a reasonable match exists
    if let Some((ram_type, speed)) = detect_ram_by_cpu(cpu_model) {
        format!("{}GB {}@{}", total_gb, ram_type, speed)
    } else {
        format!("{}GB [root permissions required]", total_gb)
    }
}

/// Real-time throughput metrics and hardware metadata for a network interface.
#[derive(Clone, Default)]
pub struct NetInterfaceInfo {
    /// Interface name (e.g. `eth0`, `wlan0`, `enp5s0`).
    pub name: String,
    /// Cumulative received bytes.
    pub rx_bytes: u64,
    /// Cumulative transmitted bytes.
    pub tx_bytes: u64,
    /// Real-time receive speed in bytes per second.
    pub rx_speed: f64,
    /// Real-time transmit speed in bytes per second.
    pub tx_speed: f64,
    /// Hardware MAC address.
    pub mac: String,
    /// Interface operational state (e.g. `up`, `down`).
    pub operstate: String,
    /// Link speed in megabits per second (Mbps).
    pub speed_mbps: u32,
    /// Link duplex mode (e.g. `Full`, `Half`, `Unknown`).
    pub duplex: String,
}

/// Reads `/proc/net/dev` and `/sys/class/net` to query network interface throughput and state.
///
/// # Arguments
/// * `prev` - Map caching previous byte counters keyed by interface name.
/// * `dt` - Time delta in seconds since the last measurement.
///
/// # Returns
/// A vector of `NetInterfaceInfo` for all physical and virtual interfaces.
pub fn read_network_interfaces(
    prev: &mut HashMap<String, (u64, u64)>,
    dt: f64,
) -> Vec<NetInterfaceInfo> {
    let mut ifaces = Vec::new();
    if let Ok(content) = fs::read_to_string("/proc/net/dev") {
        for line in content.lines().skip(2) {
            let mut parts = line.split_whitespace();
            if let Some(name_part) = parts.next() {
                let name = name_part.trim_end_matches(':').to_string();
                if name == "lo" {
                    continue;
                }
                let rx_bytes = parts
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                for _ in 0..7 {
                    parts.next();
                }
                let tx_bytes = parts
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);

                let (rx_speed, tx_speed) = if let Some(&(prev_rx, prev_tx)) = prev.get(&name) {
                    if dt > 0.0 {
                        (
                            rx_bytes.saturating_sub(prev_rx) as f64 / dt,
                            tx_bytes.saturating_sub(prev_tx) as f64 / dt,
                        )
                    } else {
                        (0.0, 0.0)
                    }
                } else {
                    (0.0, 0.0)
                };
                prev.insert(name.clone(), (rx_bytes, tx_bytes));

                let mac = fs::read_to_string(format!("/sys/class/net/{}/address", name))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let operstate = fs::read_to_string(format!("/sys/class/net/{}/operstate", name))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                let speed_mbps = fs::read_to_string(format!("/sys/class/net/{}/speed", name))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(0);
                let raw_duplex = fs::read_to_string(format!("/sys/class/net/{}/duplex", name))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let duplex = match raw_duplex.to_lowercase().as_str() {
                    "full" => "Full".to_string(),
                    "half" => "Half".to_string(),
                    _ => "Unknown".to_string(),
                };

                ifaces.push(NetInterfaceInfo {
                    name,
                    rx_bytes,
                    tx_bytes,
                    rx_speed,
                    tx_speed,
                    mac,
                    operstate,
                    speed_mbps,
                    duplex,
                });
            }
        }
    }
    ifaces
}

/// Mounted filesystem partition capacity and usage information.
#[derive(Clone, Default)]
pub struct MountInfo {
    /// Underlying block device path (e.g. `/dev/nvme0n1p2`).
    pub device: String,
    /// Destination mount point path (e.g. `/` or `/home`).
    pub mount_point: String,
    /// Filesystem type name (e.g. `ext4`, `btrfs`, `zfs`).
    pub fs_type: String,
    /// Total storage capacity in bytes.
    pub total_bytes: u64,
    /// Used storage in bytes.
    pub used_bytes: u64,
    /// Free storage available to unprivileged users in bytes.
    pub free_bytes: u64,
    /// Storage utilization percentage (0.0 to 100.0).
    pub used_pct: f64,
}

/// Individual block device model and real-time I/O throughput.
#[derive(Clone, Default)]
pub struct DiskDeviceInfo {
    /// Kernel device name (e.g. `nvme0n1` or `sda`).
    pub name: String,
    /// Disk model identifier from sysfs.
    pub model: String,
    /// Read throughput speed in bytes per second.
    pub read_speed: f64,
    /// Write throughput speed in bytes per second.
    pub write_speed: f64,
}

/// Aggregated and per-device disk I/O metrics.
#[derive(Clone, Default)]
pub struct DiskIoInfo {
    /// Aggregate read throughput in bytes per second.
    pub read_speed: f64,
    /// Aggregate write throughput in bytes per second.
    pub write_speed: f64,
    /// Total cumulative read bytes across all physical disks.
    pub total_read_bytes: u64,
    /// Total cumulative written bytes across all physical disks.
    pub total_write_bytes: u64,
    /// List of physical block devices and their individual metrics.
    pub disks: Vec<DiskDeviceInfo>,
}

/// Reads `/proc/mounts` and queries safe filesystem statistics via `rustix::fs::statvfs`.
///
/// # Returns
/// A list of `MountInfo` for physical mounted partitions.
pub fn read_disk_mounts() -> Vec<MountInfo> {
    let mut mounts = Vec::new();
    let mut seen_mounts = HashSet::new();
    if let Ok(content) = fs::read_to_string("/proc/mounts") {
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let device = parts[0];
                let mount_point = parts[1];
                let fs_type = parts[2];

                if !device.starts_with("/dev/") {
                    continue;
                }
                if fs_type == "squashfs"
                    || fs_type == "tmpfs"
                    || fs_type == "devtmpfs"
                    || fs_type == "overlay"
                {
                    continue;
                }
                if seen_mounts.contains(mount_point) {
                    continue;
                }
                seen_mounts.insert(mount_point.to_string());

                if let Ok(stat) = rustix::fs::statvfs(mount_point)
                    && stat.f_blocks > 0
                {
                    let bsize = stat.f_frsize;
                    let total_bytes = stat.f_blocks * bsize;
                    let free_bytes = stat.f_bavail * bsize;
                    let used_bytes = total_bytes.saturating_sub(stat.f_bfree * bsize);
                    let used_pct = if total_bytes > 0 {
                        (used_bytes as f64 / total_bytes as f64) * 100.0
                    } else {
                        0.0
                    };

                    mounts.push(MountInfo {
                        device: device.to_string(),
                        mount_point: mount_point.to_string(),
                        fs_type: fs_type.to_string(),
                        total_bytes,
                        used_bytes,
                        free_bytes,
                        used_pct,
                    });
                }
            }
        }
    }
    mounts.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    mounts
}

/// Reads `/proc/diskstats` and `/sys/block` to track global and per-drive disk throughput.
///
/// # Arguments
/// * `prev` - Map caching previous sector counts keyed by device name.
/// * `dt` - Time delta in seconds since the last measurement.
///
/// # Returns
/// A populated `DiskIoInfo` structure.
pub fn read_disk_io(prev: &mut HashMap<String, (u64, u64)>, dt: f64) -> DiskIoInfo {
    let mut info = DiskIoInfo::default();
    if let Ok(content) = fs::read_to_string("/proc/diskstats") {
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 10 {
                let name = parts[2];
                let is_disk = (name.starts_with("nvme") && !name.contains('p'))
                    || ((name.starts_with("sd")
                        || name.starts_with("vd")
                        || name.starts_with("hd"))
                        && !name.chars().last().unwrap_or(' ').is_ascii_digit());

                if !is_disk {
                    continue;
                }

                let read_sectors = parts[5].parse::<u64>().unwrap_or(0);
                let write_sectors = parts[9].parse::<u64>().unwrap_or(0);
                let read_bytes = read_sectors * 512;
                let write_bytes = write_sectors * 512;

                info.total_read_bytes += read_bytes;
                info.total_write_bytes += write_bytes;

                let (d_rx_speed, d_tx_speed) = if let Some(&(prev_r, prev_w)) = prev.get(name) {
                    if dt > 0.0 {
                        (
                            read_bytes.saturating_sub(prev_r) as f64 / dt,
                            write_bytes.saturating_sub(prev_w) as f64 / dt,
                        )
                    } else {
                        (0.0, 0.0)
                    }
                } else {
                    (0.0, 0.0)
                };
                prev.insert(name.to_string(), (read_bytes, write_bytes));

                info.read_speed += d_rx_speed;
                info.write_speed += d_tx_speed;

                let model = fs::read_to_string(format!("/sys/block/{}/device/model", name))
                    .or_else(|_| fs::read_to_string(format!("/sys/block/{}/device/name", name)))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| name.to_string());

                info.disks.push(DiskDeviceInfo {
                    name: name.to_string(),
                    model,
                    read_speed: d_rx_speed,
                    write_speed: d_tx_speed,
                });
            }
        }
    }
    info
}

/// Detailed information about a Docker named or anonymous storage volume.
#[derive(Clone, Default)]
pub struct DockerVolumeInfo {
    /// Volume name or hash identifier.
    pub name: String,
    /// Number of active container links/mounts.
    pub links: u32,
    /// Storage space used by the volume in bytes.
    pub size_bytes: u64,
    /// Formatted human-readable size string.
    pub size_str: String,
}

/// Overall Docker / Container engine disk space telemetry.
#[derive(Clone, Default)]
pub struct DockerStorageInfo {
    /// Whether Docker daemon is available and responding.
    pub is_available: bool,
    /// Total storage used across all local volumes in bytes.
    pub total_volumes_bytes: u64,
    /// Formatted total volumes size string.
    pub total_volumes_str: String,
    /// Formatted total build cache size string.
    pub total_build_cache_str: String,
    /// Individual volume details.
    pub volumes: Vec<DockerVolumeInfo>,
}

/// Queries Docker daemon via `docker system df -v` to extract image, container,
/// build cache, and per-volume disk space usage metrics.
///
/// # Returns
/// A populated `DockerStorageInfo` structure.
pub fn read_docker_storage() -> DockerStorageInfo {
    let mut info = DockerStorageInfo::default();

    for bin in &[
        "docker",
        "/run/current-system/sw/bin/docker",
        "/usr/bin/docker",
        "/usr/local/bin/docker",
    ] {
        if let Ok(output) = std::process::Command::new(bin)
            .args(["system", "df", "-v"])
            .output()
            && output.status.success()
        {
            info.is_available = true;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut section = "";

            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Images space usage:") {
                    section = "images";
                    continue;
                } else if trimmed.starts_with("Containers space usage:") {
                    section = "containers";
                    continue;
                } else if trimmed.starts_with("Local Volumes space usage:") {
                    section = "volumes";
                    continue;
                } else if trimmed.starts_with("Build cache usage:") {
                    section = "cache";
                    let cache_str = trimmed.trim_start_matches("Build cache usage:").trim();
                    info.total_build_cache_str = cache_str.to_string();
                    continue;
                }

                if section == "volumes" {
                    if trimmed.is_empty() || trimmed.starts_with("VOLUME NAME") {
                        continue;
                    }
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    if parts.len() >= 3 {
                        let name = parts[0].to_string();
                        let links = parts[1].parse::<u32>().unwrap_or(0);
                        let size_str = parts[2].to_string();
                        let size_bytes = parse_size_to_bytes(&size_str);
                        info.total_volumes_bytes += size_bytes;
                        info.volumes.push(DockerVolumeInfo {
                            name,
                            links,
                            size_bytes,
                            size_str,
                        });
                    }
                }
            }

            info.total_volumes_str = format_bytes_dyn(info.total_volumes_bytes as f64);
            return info;
        }
    }

    info
}

/// Safely and recursively calculates the total disk space in bytes of a directory.
///
/// # Arguments
/// * `path` - Directory path to traverse.
///
/// # Returns
/// Cumulative byte size.
pub fn get_dir_size<P: AsRef<std::path::Path>>(path: P) -> u64 {
    let p = path.as_ref();
    for bin in &["du", "/run/current-system/sw/bin/du", "/usr/bin/du"] {
        if let Ok(output) = std::process::Command::new(bin)
            .args(["-sb", p.to_string_lossy().as_ref()])
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_word) = stdout.split_whitespace().next()
                && let Ok(bytes) = first_word.parse::<u64>()
            {
                return bytes;
            }
        }
    }

    let mut total = 0;
    if let Ok(entries) = fs::read_dir(p) {
        for entry in entries.filter_map(Result::ok) {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += get_dir_size(entry.path());
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// An individual package, prefix, cache, volume, or directory item within a storage category.
#[derive(Clone, Default)]
pub struct PackageStorageItem {
    /// Name or identifier of the item.
    pub name: String,
    /// Detailed description or sub-label.
    pub detail: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Formatted human-readable size string.
    pub size_str: String,
    /// Absolute filesystem path (used for expandable tree navigation).
    pub path: String,
    /// Whether this item is a directory that can be expanded.
    pub is_dir: bool,
    /// Whether this directory item is currently expanded in the tree view.
    pub is_expanded: bool,
    /// Whether this directory is currently being scanned in the background.
    pub is_scanning: bool,
    /// Indentation depth level in the file tree (0 for root items).
    pub depth: usize,
}

impl PackageStorageItem {
    /// Creates a new storage item with default tree properties.
    #[allow(dead_code)]
    pub fn new(
        name: impl Into<String>,
        detail: impl Into<String>,
        size_bytes: u64,
        size_str: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            size_bytes,
            size_str: size_str.into(),
            path: String::new(),
            is_dir: false,
            is_expanded: false,
            is_scanning: false,
            depth: 0,
        }
    }
}

/// Disk space metrics for a detected package manager, container engine, or runtime environment.
#[derive(Clone, Default)]
pub struct PackageStorageCategory {
    /// Category display title (e.g. "All", "Docker", "Wine", "Flatpak", "Snap", "Nix Store", "APT", "DNF", "Pacman", "Cargo", "npm").
    pub name: String,
    /// Formatted total storage size string.
    pub total_str: String,
    /// List of individual volume, package, prefix, cache, or directory components.
    pub items: Vec<PackageStorageItem>,
}

/// Parses the stdout from `dust` into a `PackageStorageCategory`.
///
/// # Arguments
/// * `stdout` - Raw output from `dust -d 1 -c /`.
///
/// # Returns
/// An optional `PackageStorageCategory` representing the `"All"` disk breakdown.
pub fn parse_dust_output(stdout: &str) -> Option<PackageStorageCategory> {
    let mut items = Vec::new();
    let mut total_bytes = 0;
    let mut total_str = "0 B".to_string();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let size_tok = parts.next().unwrap_or("");
        let size_bytes = parse_size_to_bytes(size_tok);

        let rest = if let Some(idx) = trimmed.find(char::is_whitespace) {
            trimmed[idx..].trim_start()
        } else {
            ""
        };

        let without_tree = rest
            .trim_start_matches(|c| {
                c == '┌' || c == '├' || c == '└' || c == '─' || c == '┴' || c == '│' || c == ' '
            })
            .trim();

        let dir_name = if let Some(pipe_idx) = without_tree.find('│') {
            without_tree[..pipe_idx].trim()
        } else if let Some(bar_idx) = without_tree.find('|') {
            without_tree[..bar_idx].trim()
        } else {
            without_tree.split_whitespace().next().unwrap_or("")
        };

        if dir_name.is_empty() {
            continue;
        }

        let pct_str = if let Some(last_pipe) = trimmed.rfind('│') {
            trimmed[last_pipe + '│'.len_utf8()..].trim().to_string()
        } else if let Some(last_bar) = trimmed.rfind('|') {
            trimmed[last_bar + 1..].trim().to_string()
        } else {
            String::new()
        };

        let full_path = if dir_name == "/" {
            "/".to_string()
        } else {
            format!("/{}", dir_name)
        };

        if dir_name == "/" {
            total_bytes = size_bytes;
            total_str = format_bytes_dyn(size_bytes as f64);
        } else {
            let detail = pct_str;

            items.push(PackageStorageItem {
                name: full_path.clone(),
                detail,
                size_bytes,
                size_str: format_bytes_dyn(size_bytes as f64),
                path: full_path,
                is_dir: true,
                is_expanded: false,
                is_scanning: false,
                depth: 0,
            });
        }
    }

    if !items.is_empty() || total_bytes > 0 {
        items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        Some(PackageStorageCategory {
            name: "All".to_string(),
            total_str: if total_bytes > 0 {
                total_str
            } else {
                format_bytes_dyn(items.iter().map(|i| i.size_bytes).sum::<u64>() as f64)
            },
            items,
        })
    } else {
        None
    }
}

/// Parses child directory items from `dust -d 1 -c <parent_path>` stdout.
///
/// # Arguments
/// * `parent_path` - The parent directory path being expanded.
/// * `parent_depth` - The indentation depth of the parent directory.
/// * `stdout` - Raw output from `dust`.
///
/// # Returns
/// A vector of child `PackageStorageItem`s.
pub fn parse_dust_children(
    parent_path: &str,
    parent_depth: usize,
    stdout: &str,
) -> Vec<PackageStorageItem> {
    let mut items = Vec::new();
    let norm_parent = parent_path.trim_end_matches('/');
    let parent_base = std::path::Path::new(norm_parent)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| norm_parent.to_string());

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let size_tok = parts.next().unwrap_or("");
        let size_bytes = parse_size_to_bytes(size_tok);

        let rest = if let Some(idx) = trimmed.find(char::is_whitespace) {
            trimmed[idx..].trim_start()
        } else {
            ""
        };

        let without_tree = rest
            .trim_start_matches(|c| {
                c == '┌' || c == '├' || c == '└' || c == '─' || c == '┴' || c == '│' || c == ' '
            })
            .trim();

        let dir_name = if let Some(pipe_idx) = without_tree.find('│') {
            without_tree[..pipe_idx].trim()
        } else if let Some(bar_idx) = without_tree.find('|') {
            without_tree[..bar_idx].trim()
        } else {
            without_tree.split_whitespace().next().unwrap_or("")
        };

        if dir_name.is_empty() {
            continue;
        }

        // If this line is the parent summary (e.g. ┌─┴ home or matches parent base), skip it
        if trimmed.contains("┌─┴") || dir_name == parent_base || dir_name == norm_parent {
            continue;
        }

        let pct_str = if let Some(last_pipe) = trimmed.rfind('│') {
            trimmed[last_pipe + '│'.len_utf8()..].trim().to_string()
        } else if let Some(last_bar) = trimmed.rfind('|') {
            trimmed[last_bar + 1..].trim().to_string()
        } else {
            String::new()
        };

        let child_path = if norm_parent.is_empty() {
            format!("/{}", dir_name)
        } else {
            format!("{}/{}", norm_parent, dir_name)
        };
        let is_dir = std::path::Path::new(&child_path).is_dir();

        let detail = pct_str;

        items.push(PackageStorageItem {
            name: dir_name.to_string(),
            detail,
            size_bytes,
            size_str: format_bytes_dyn(size_bytes as f64),
            path: child_path,
            is_dir,
            is_expanded: false,
            is_scanning: false,
            depth: parent_depth + 1,
        });
    }

    items.sort_by(|a, b| {
        b.size_bytes
            .cmp(&a.size_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });

    items
}

/// Determines if a file/directory item in the storage tree view should be displayed
/// based on the hidden files and minimum size (4.0 KB) filter.
///
/// Edge case: if a folder/file is hidden (starts with `.`) but has at least 1 GB
/// (`size_bytes >= 1024 * 1024 * 1024`), it is displayed even when hidden files are toggled off.
///
/// # Arguments
/// * `item` - The item to evaluate.
/// * `show_hidden` - If true, displays all items including hidden and < 4.0 KB files.
///
/// # Returns
/// `true` if the item is visible under the current filter settings.
pub fn is_item_visible(item: &PackageStorageItem, show_hidden: bool) -> bool {
    if show_hidden {
        return true;
    }
    // Files/folders of 4.0 KB or smaller (e.g. empty directories like /root) are hidden by default
    if item.size_bytes <= 4096 {
        return false;
    }
    let is_hidden = if item.depth == 0 {
        item.name.trim_start_matches('/').starts_with('.')
    } else {
        item.name.starts_with('.')
    };
    // Edge case: if hidden (starts with .) but has at least 1 GB (1 GiB = 1024^3 bytes), show it
    if is_hidden && item.size_bytes < 1024 * 1024 * 1024 {
        return false;
    }
    true
}

/// Scans an individual directory path using `dust` and returns its child items.
///
/// # Arguments
/// * `parent_path` - Directory to scan (e.g. `"/home"`, `"/var"`).
/// * `parent_depth` - Indentation depth of the parent item in the tree.
///
/// # Returns
/// A sorted vector of child `PackageStorageItem`s with `depth = parent_depth + 1`.
pub fn read_dust_path(parent_path: &str, parent_depth: usize) -> Vec<PackageStorageItem> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let dust_bins = [
        "dust",
        "/run/current-system/sw/bin/dust",
        "/usr/bin/dust",
        &format!("{}/.nix-profile/bin/dust", home),
        &format!("{}/.cargo/bin/dust", home),
    ];

    for bin in dust_bins {
        if let Ok(output) = std::process::Command::new(bin)
            .args(["-d", "1", "-c", parent_path])
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            return parse_dust_children(parent_path, parent_depth, &stdout);
        }
    }

    Vec::new()
}

/// Scans the entire filesystem root breakdown using the `dust` CLI.
///
/// # Returns
/// An optional `PackageStorageCategory` representing the `"All"` disk breakdown.
pub fn read_dust_storage() -> Option<PackageStorageCategory> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let dust_bins = [
        "dust",
        "/run/current-system/sw/bin/dust",
        "/usr/bin/dust",
        &format!("{}/.nix-profile/bin/dust", home),
        &format!("{}/.cargo/bin/dust", home),
    ];

    for bin in dust_bins {
        if let Ok(output) = std::process::Command::new(bin)
            .args(["-d", "1", "-c", "/"])
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(cat) = parse_dust_output(&stdout) {
                return Some(cat);
            }
        }
    }

    None
}

/// Scans, parses, and aggregates storage space metrics across all detected system package managers,
/// container storage directories, and developer caches.
///
/// # Returns
/// A vector of `PackageStorageCategory` for all detected systems.
pub fn read_package_storage_categories() -> Vec<PackageStorageCategory> {
    let mut categories = Vec::new();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());

    // 1. Docker Volumes
    let docker_info = read_docker_storage();
    if docker_info.is_available
        && (!docker_info.volumes.is_empty() || docker_info.total_volumes_bytes > 0)
    {
        let mut items = Vec::new();
        for vol in docker_info.volumes {
            items.push(PackageStorageItem {
                name: vol.name,
                detail: if vol.links == 1 {
                    "1 link".to_string()
                } else {
                    format!("{} links", vol.links)
                },
                size_bytes: vol.size_bytes,
                size_str: vol.size_str,
                ..Default::default()
            });
        }
        items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        categories.push(PackageStorageCategory {
            name: "Docker".to_string(),
            total_str: docker_info.total_volumes_str,
            items,
        });
    }

    // 2. Wine Prefixes
    let mut wine_items = Vec::new();
    let mut wine_total = 0;
    let mut prefix_dirs = Vec::new();

    let default_wine = format!("{}/.wine", home);
    if std::path::Path::new(&default_wine).exists() {
        prefix_dirs.push(("Default (~/.wine)".to_string(), default_wine.clone()));
    }
    if let Ok(wp) = std::env::var("WINEPREFIX")
        && !wp.is_empty()
        && std::path::Path::new(&wp).exists()
        && wp != default_wine
    {
        prefix_dirs.push(("Custom ($WINEPREFIX)".to_string(), wp));
    }
    let share_prefixes = format!("{}/.local/share/wineprefixes", home);
    if let Ok(entries) = fs::read_dir(&share_prefixes) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() && (p.join("drive_c").exists() || p.join("system.reg").exists()) {
                let name = format!("wineprefix/{}", entry.file_name().to_string_lossy());
                prefix_dirs.push((name, p.to_string_lossy().to_string()));
            }
        }
    }
    let vinegar_prefixes = format!("{}/.local/share/vinegar/prefixes", home);
    if let Ok(entries) = fs::read_dir(&vinegar_prefixes) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() && (p.join("drive_c").exists() || p.join("system.reg").exists()) {
                let name = format!("vinegar/{}", entry.file_name().to_string_lossy());
                prefix_dirs.push((name, p.to_string_lossy().to_string()));
            }
        }
    }
    for b_path in &[
        format!("{}/.local/share/bottles/bottles", home),
        format!(
            "{}/.var/app/com.usebottles.bottles/data/bottles/bottles",
            home
        ),
    ] {
        if let Ok(entries) = fs::read_dir(b_path) {
            for entry in entries.filter_map(Result::ok) {
                let p = entry.path();
                if p.is_dir() && (p.join("drive_c").exists() || p.join("system.reg").exists()) {
                    let name = format!("bottle/{}", entry.file_name().to_string_lossy());
                    prefix_dirs.push((name, p.to_string_lossy().to_string()));
                }
            }
        }
    }
    let lutris_prefixes = format!("{}/.local/share/lutris/prefixes", home);
    if let Ok(entries) = fs::read_dir(&lutris_prefixes) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() && (p.join("drive_c").exists() || p.join("system.reg").exists()) {
                let name = format!("lutris/{}", entry.file_name().to_string_lossy());
                prefix_dirs.push((name, p.to_string_lossy().to_string()));
            }
        }
    }

    for (name, path_str) in prefix_dirs {
        let size = get_dir_size(&path_str);
        if size > 0 {
            wine_total += size;
            wine_items.push(PackageStorageItem {
                name,
                detail: path_str,
                size_bytes: size,
                size_str: format_bytes_dyn(size as f64),
                ..Default::default()
            });
        }
    }

    if !wine_items.is_empty() {
        wine_items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        categories.push(PackageStorageCategory {
            name: "Wine".to_string(),
            total_str: format_bytes_dyn(wine_total as f64),
            items: wine_items,
        });
    }

    // 3. Flatpak
    let mut flatpak_items = Vec::new();
    let mut flatpak_total = 0;
    for bin in &[
        "flatpak",
        "/run/current-system/sw/bin/flatpak",
        "/usr/bin/flatpak",
    ] {
        if let Ok(output) = std::process::Command::new(bin)
            .args(["list", "--columns=application,size,runtime"])
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().skip(1) {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    let app_id = parts[0];
                    let size_str = parts[1].replace(',', ".");
                    let size = parse_size_to_bytes(&size_str);
                    let is_runtime = parts.len() >= 3 && !parts[2].is_empty();
                    let detail = if is_runtime {
                        "Runtime".to_string()
                    } else {
                        "Application".to_string()
                    };
                    if size > 0 {
                        flatpak_total += size;
                        flatpak_items.push(PackageStorageItem {
                            name: app_id.to_string(),
                            detail,
                            size_bytes: size,
                            size_str: format_bytes_dyn(size as f64),
                            ..Default::default()
                        });
                    }
                }
            }
            break;
        }
    }
    let var_app = format!("{}/.var/app", home);
    if std::path::Path::new(&var_app).exists() {
        let app_data_size = get_dir_size(&var_app);
        if app_data_size > 0 {
            flatpak_total += app_data_size;
            flatpak_items.push(PackageStorageItem {
                name: "Flatpak App Data (~/.var/app)".to_string(),
                detail: "Per-application storage & config".to_string(),
                size_bytes: app_data_size,
                size_str: format_bytes_dyn(app_data_size as f64),
                ..Default::default()
            });
        }
    }
    if !flatpak_items.is_empty() || std::path::Path::new("/var/lib/flatpak").exists() {
        if flatpak_items.is_empty() {
            let sys_flatpak = get_dir_size("/var/lib/flatpak");
            if sys_flatpak > 0 {
                flatpak_total += sys_flatpak;
                flatpak_items.push(PackageStorageItem {
                    name: "System Flatpak Runtimes".to_string(),
                    detail: "/var/lib/flatpak".to_string(),
                    size_bytes: sys_flatpak,
                    size_str: format_bytes_dyn(sys_flatpak as f64),
                    ..Default::default()
                });
            }
        }
        if flatpak_total > 0 {
            flatpak_items.sort_by(|a, b| {
                b.size_bytes
                    .cmp(&a.size_bytes)
                    .then_with(|| a.name.cmp(&b.name))
            });
            categories.push(PackageStorageCategory {
                name: "Flatpak".to_string(),
                total_str: format_bytes_dyn(flatpak_total as f64),
                items: flatpak_items,
            });
        }
    }

    // 4. Snap
    let mut snap_items = Vec::new();
    let mut snap_total = 0;
    for bin in &["snap", "/run/current-system/sw/bin/snap", "/usr/bin/snap"] {
        if let Ok(output) = std::process::Command::new(bin).args(["list"]).output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    let name = parts[0].to_string();
                    let rev = parts[2].to_string();
                    let publisher = parts[3].to_string();
                    snap_items.push(PackageStorageItem {
                        name: format!("{} (rev {})", name, rev),
                        detail: format!("Publisher: {}", publisher),
                        size_bytes: 0,
                        size_str: "Installed".to_string(),
                        ..Default::default()
                    });
                }
            }
            break;
        }
    }
    for snap_dir in &["/var/lib/snapd/snaps", &format!("{}/snap", home)] {
        if std::path::Path::new(snap_dir).exists() {
            let size = get_dir_size(snap_dir);
            if size > 0 {
                snap_total += size;
                snap_items.push(PackageStorageItem {
                    name: format!("Snap Data ({})", snap_dir),
                    detail: snap_dir.to_string(),
                    size_bytes: size,
                    size_str: format_bytes_dyn(size as f64),
                    ..Default::default()
                });
            }
        }
    }
    if snap_total > 0 || !snap_items.is_empty() {
        snap_items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        categories.push(PackageStorageCategory {
            name: "Snap".to_string(),
            total_str: format_bytes_dyn(snap_total as f64),
            items: snap_items,
        });
    }

    // 5. NixOS / Nix Store
    if std::path::Path::new("/nix/store").exists() {
        let mut path_count = 0;
        if let Ok(entries) = fs::read_dir("/nix/store") {
            path_count = entries.count();
        }
        let nix_size = if let Ok(stat) = rustix::fs::statvfs("/nix/store") {
            let total_bytes = stat.f_blocks * stat.f_frsize;
            let free_bytes = stat.f_bfree * stat.f_frsize;
            total_bytes.saturating_sub(free_bytes)
        } else {
            get_dir_size("/nix/store")
        };

        let mut items = Vec::new();
        let mut seen_paths = HashSet::new();

        // Enumerate installed packages from system and user profiles
        for bin_dir in &[
            "/run/current-system/sw/bin",
            "/nix/var/nix/profiles/system/sw/bin",
            &format!("{}/.nix-profile/bin", home),
        ] {
            if let Ok(entries) = fs::read_dir(bin_dir) {
                for entry in entries.filter_map(Result::ok) {
                    if let Ok(target) = fs::read_link(entry.path()) {
                        let target_path = if target.is_relative() {
                            entry
                                .path()
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new("/"))
                                .join(target)
                        } else {
                            target
                        };

                        if let Some(store_str) = target_path.to_str()
                            && store_str.starts_with("/nix/store/")
                        {
                            let rel = &store_str["/nix/store/".len()..];
                            let pkg_dir_name = rel.split('/').next().unwrap_or("");
                            let pkg_root = format!("/nix/store/{}", pkg_dir_name);

                            if !seen_paths.contains(&pkg_root)
                                && std::path::Path::new(&pkg_root).exists()
                            {
                                seen_paths.insert(pkg_root.clone());
                                let size = get_dir_size(&pkg_root);
                                let clean_name = if pkg_dir_name.len() > 33
                                    && pkg_dir_name.as_bytes()[32] == b'-'
                                {
                                    &pkg_dir_name[33..]
                                } else {
                                    pkg_dir_name
                                };

                                items.push(PackageStorageItem {
                                    name: clean_name.to_string(),
                                    detail: pkg_root,
                                    size_bytes: size,
                                    size_str: format_bytes_dyn(size as f64),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }

        // Add System Generations
        if let Ok(entries) = fs::read_dir("/nix/var/nix/profiles") {
            for entry in entries.filter_map(Result::ok) {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with("system-")
                    && fname.ends_with("-link")
                    && let Ok(target) = fs::read_link(entry.path())
                {
                    let target_str = target.to_string_lossy().to_string();
                    let size = get_dir_size(&target_str);
                    items.push(PackageStorageItem {
                        name: format!("Generation {}", fname.trim_end_matches("-link")),
                        detail: target_str,
                        size_bytes: size,
                        size_str: format_bytes_dyn(size as f64),
                        ..Default::default()
                    });
                }
            }
        }

        // Add Nix Store summary item
        items.push(PackageStorageItem {
            name: "Nix Store Total Partition".to_string(),
            detail: format!("{} total store paths indexed", path_count),
            size_bytes: nix_size,
            size_str: format_bytes_dyn(nix_size as f64),
            ..Default::default()
        });

        items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });

        categories.push(PackageStorageCategory {
            name: "Nix Store".to_string(),
            total_str: format_bytes_dyn(nix_size as f64),
            items,
        });
    }

    // 6. APT
    if std::path::Path::new("/var/cache/apt/archives").exists()
        || std::path::Path::new("/var/lib/dpkg").exists()
    {
        let mut items = Vec::new();
        let mut apt_total = 0;
        let archives_size = get_dir_size("/var/cache/apt/archives");
        if archives_size > 0 {
            apt_total += archives_size;
            items.push(PackageStorageItem {
                name: "APT Package Cache".to_string(),
                detail: "/var/cache/apt/archives".to_string(),
                size_bytes: archives_size,
                size_str: format_bytes_dyn(archives_size as f64),
                ..Default::default()
            });
        }
        let dpkg_size = get_dir_size("/var/lib/dpkg");
        if dpkg_size > 0 {
            apt_total += dpkg_size;
            items.push(PackageStorageItem {
                name: "DPKG Database & Status".to_string(),
                detail: "/var/lib/dpkg".to_string(),
                size_bytes: dpkg_size,
                size_str: format_bytes_dyn(dpkg_size as f64),
                ..Default::default()
            });
        }
        if apt_total > 0 || !items.is_empty() {
            items.sort_by(|a, b| {
                b.size_bytes
                    .cmp(&a.size_bytes)
                    .then_with(|| a.name.cmp(&b.name))
            });
            categories.push(PackageStorageCategory {
                name: "APT".to_string(),
                total_str: format_bytes_dyn(apt_total as f64),
                items,
            });
        }
    }

    // 7. DNF
    if std::path::Path::new("/var/cache/dnf").exists()
        || std::path::Path::new("/var/lib/dnf").exists()
        || std::path::Path::new("/var/cache/yum").exists()
    {
        let mut items = Vec::new();
        let mut dnf_total = 0;
        for path in &["/var/cache/dnf", "/var/cache/yum", "/var/lib/dnf"] {
            if std::path::Path::new(path).exists() {
                let size = get_dir_size(path);
                if size > 0 {
                    dnf_total += size;
                    items.push(PackageStorageItem {
                        name: format!("DNF Storage ({})", path),
                        detail: path.to_string(),
                        size_bytes: size,
                        size_str: format_bytes_dyn(size as f64),
                        ..Default::default()
                    });
                }
            }
        }
        if dnf_total > 0 {
            items.sort_by(|a, b| {
                b.size_bytes
                    .cmp(&a.size_bytes)
                    .then_with(|| a.name.cmp(&b.name))
            });
            categories.push(PackageStorageCategory {
                name: "DNF".to_string(),
                total_str: format_bytes_dyn(dnf_total as f64),
                items,
            });
        }
    }

    // 8. Pacman
    if std::path::Path::new("/var/cache/pacman/pkg").exists()
        || std::path::Path::new("/var/lib/pacman").exists()
    {
        let mut items = Vec::new();
        let mut pac_total = 0;
        let pkg_size = get_dir_size("/var/cache/pacman/pkg");
        if pkg_size > 0 {
            pac_total += pkg_size;
            items.push(PackageStorageItem {
                name: "Pacman Package Cache".to_string(),
                detail: "/var/cache/pacman/pkg".to_string(),
                size_bytes: pkg_size,
                size_str: format_bytes_dyn(pkg_size as f64),
                ..Default::default()
            });
        }
        let db_size = get_dir_size("/var/lib/pacman");
        if db_size > 0 {
            pac_total += db_size;
            items.push(PackageStorageItem {
                name: "Pacman Local Database".to_string(),
                detail: "/var/lib/pacman".to_string(),
                size_bytes: db_size,
                size_str: format_bytes_dyn(db_size as f64),
                ..Default::default()
            });
        }
        if pac_total > 0 || !items.is_empty() {
            items.sort_by(|a, b| {
                b.size_bytes
                    .cmp(&a.size_bytes)
                    .then_with(|| a.name.cmp(&b.name))
            });
            categories.push(PackageStorageCategory {
                name: "Pacman".to_string(),
                total_str: format_bytes_dyn(pac_total as f64),
                items,
            });
        }
    }

    // 9. Podman Container Storage
    let mut podman_items = Vec::new();
    let mut podman_total = 0;
    let podman_rootless = format!("{}/.local/share/containers/storage", home);
    for (root, kind) in &[
        (podman_rootless.as_str(), "User (~/.local/share/containers)"),
        (
            "/var/lib/containers/storage",
            "System (/var/lib/containers)",
        ),
    ] {
        if std::path::Path::new(root).exists() {
            let subdirs = [
                ("overlay-images", "Image Layers & Manifests"),
                ("overlay-containers", "Container Layers & Configs"),
                ("volumes", "Named Data Volumes"),
                ("overlay", "Storage Overlayfs Rootfs"),
                ("mounts", "Bind & Ephemeral Mounts"),
            ];
            for (subdir, desc) in subdirs {
                let p = format!("{}/{}", root, subdir);
                if std::path::Path::new(&p).exists() {
                    let size = get_dir_size(&p);
                    if size > 0 {
                        podman_total += size;
                        podman_items.push(PackageStorageItem {
                            name: format!(
                                "{}: {}",
                                if root.starts_with("/var") {
                                    "sys"
                                } else {
                                    "user"
                                },
                                subdir
                            ),
                            detail: format!("{} ({})", desc, kind),
                            size_bytes: size,
                            size_str: format_bytes_dyn(size as f64),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    if podman_total > 0 || !podman_items.is_empty() {
        podman_items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        categories.push(PackageStorageCategory {
            name: "Podman".to_string(),
            total_str: format_bytes_dyn(podman_total as f64),
            items: podman_items,
        });
    }

    // 10. Cargo & Rust Toolchains
    let cargo_home = format!("{}/.cargo", home);
    let rustup_home = format!("{}/.rustup", home);
    let mut cargo_items = Vec::new();
    let mut cargo_total = 0;

    // Installed binaries in ~/.cargo/bin
    let cargo_bin = format!("{}/bin", cargo_home);
    if let Ok(entries) = fs::read_dir(&cargo_bin) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_file()
                && let Ok(meta) = p.metadata()
            {
                let size = meta.len();
                if size > 0 {
                    cargo_total += size;
                    let name = entry.file_name().to_string_lossy().to_string();
                    cargo_items.push(PackageStorageItem {
                        name: format!("bin: {}", name),
                        detail: format!("Installed CLI Binary ({})", p.to_string_lossy()),
                        size_bytes: size,
                        size_str: format_bytes_dyn(size as f64),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // Downloaded .crate archives in ~/.cargo/registry/cache/*
    let registry_cache = format!("{}/registry/cache", cargo_home);
    if let Ok(entries) = fs::read_dir(&registry_cache) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() {
                let cache_dir_size = get_dir_size(&p);
                if cache_dir_size > 0 {
                    cargo_total += cache_dir_size;
                    let reg_name = entry.file_name().to_string_lossy().to_string();
                    let clean_reg = if let Some(idx) = reg_name.find('-') {
                        &reg_name[..idx]
                    } else {
                        &reg_name
                    };
                    let count = fs::read_dir(&p).map(|d| d.count()).unwrap_or(0);
                    cargo_items.push(PackageStorageItem {
                        name: format!("crates.io cache ({})", clean_reg),
                        detail: format!("{} downloaded .crate packages", count),
                        size_bytes: cache_dir_size,
                        size_str: format_bytes_dyn(cache_dir_size as f64),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // Extracted crate sources in ~/.cargo/registry/src/*
    let registry_src = format!("{}/registry/src", cargo_home);
    if std::path::Path::new(&registry_src).exists() {
        let src_size = get_dir_size(&registry_src);
        if src_size > 0 {
            cargo_total += src_size;
            cargo_items.push(PackageStorageItem {
                name: "registry/src (Unpacked Sources)".to_string(),
                detail: registry_src,
                size_bytes: src_size,
                size_str: format_bytes_dyn(src_size as f64),
                ..Default::default()
            });
        }
    }

    // Git dependency repositories in ~/.cargo/git
    let cargo_git = format!("{}/git", cargo_home);
    if std::path::Path::new(&cargo_git).exists() {
        let git_size = get_dir_size(&cargo_git);
        if git_size > 0 {
            cargo_total += git_size;
            cargo_items.push(PackageStorageItem {
                name: "git/db (Git Dependencies)".to_string(),
                detail: cargo_git,
                size_bytes: git_size,
                size_str: format_bytes_dyn(git_size as f64),
                ..Default::default()
            });
        }
    }

    // Rustup toolchains in ~/.rustup/toolchains/*
    let toolchains_dir = format!("{}/toolchains", rustup_home);
    if let Ok(entries) = fs::read_dir(&toolchains_dir) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() {
                let size = get_dir_size(&p);
                if size > 0 {
                    cargo_total += size;
                    let tc_name = entry.file_name().to_string_lossy().to_string();
                    cargo_items.push(PackageStorageItem {
                        name: format!("toolchain: {}", tc_name),
                        detail: format!(
                            "Rust compiler & standard library ({})",
                            p.to_string_lossy()
                        ),
                        size_bytes: size,
                        size_str: format_bytes_dyn(size as f64),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // Shared compiler cache ~/.cache/sccache
    let sccache_dir = format!("{}/.cache/sccache", home);
    if std::path::Path::new(&sccache_dir).exists() {
        let sc_size = get_dir_size(&sccache_dir);
        if sc_size > 0 {
            cargo_total += sc_size;
            cargo_items.push(PackageStorageItem {
                name: "sccache (Compilation Cache)".to_string(),
                detail: sccache_dir,
                size_bytes: sc_size,
                size_str: format_bytes_dyn(sc_size as f64),
                ..Default::default()
            });
        }
    }

    if cargo_total > 0 || !cargo_items.is_empty() {
        cargo_items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        categories.push(PackageStorageCategory {
            name: "Cargo".to_string(),
            total_str: format_bytes_dyn(cargo_total as f64),
            items: cargo_items,
        });
    }

    // 11. npm & Node.js Packages
    let mut npm_items = Vec::new();
    let mut npm_total = 0;

    // Cache directories
    let npm_cache = format!("{}/.npm", home);
    if std::path::Path::new(&npm_cache).exists() {
        let cacache = format!("{}/_cacache", npm_cache);
        let cacache_size = get_dir_size(&cacache);
        if cacache_size > 0 {
            npm_total += cacache_size;
            npm_items.push(PackageStorageItem {
                name: "npm Content Cache (~/.npm/_cacache)".to_string(),
                detail: "HTTP responses, tarballs & metadata index".to_string(),
                size_bytes: cacache_size,
                size_str: format_bytes_dyn(cacache_size as f64),
                ..Default::default()
            });
        }

        // Scan npx cached packages in ~/.npm/_npx/*/node_modules/*
        let npx_dir = format!("{}/_npx", npm_cache);
        if let Ok(entries) = fs::read_dir(&npx_dir) {
            for hash_entry in entries.filter_map(Result::ok) {
                let nm = hash_entry.path().join("node_modules");
                if let Ok(sub_entries) = fs::read_dir(&nm) {
                    for pkg_entry in sub_entries.filter_map(Result::ok) {
                        let p = pkg_entry.path();
                        if p.is_dir() {
                            let name = pkg_entry.file_name().to_string_lossy().to_string();
                            if name.starts_with('.') {
                                continue;
                            }
                            let size = get_dir_size(&p);
                            if size > 0 {
                                npm_total += size;
                                npm_items.push(PackageStorageItem {
                                    name: format!("npx: {}", name),
                                    detail: format!(
                                        "Cached npx runner package ({})",
                                        p.to_string_lossy()
                                    ),
                                    size_bytes: size,
                                    size_str: format_bytes_dyn(size as f64),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }

        let logs_cache = format!("{}/_logs", npm_cache);
        let logs_size = get_dir_size(&logs_cache);
        if logs_size > 0 {
            npm_total += logs_size;
            npm_items.push(PackageStorageItem {
                name: "npm Debug Logs (~/.npm/_logs)".to_string(),
                detail: "CLI run logs".to_string(),
                size_bytes: logs_size,
                size_str: format_bytes_dyn(logs_size as f64),
                ..Default::default()
            });
        }
    }

    // Scan individual global node_modules packages
    for node_mod_dir in &[
        format!("{}/.npm-global/lib/node_modules", home),
        "/usr/lib/node_modules".to_string(),
        "/usr/local/lib/node_modules".to_string(),
    ] {
        if let Ok(entries) = fs::read_dir(node_mod_dir) {
            for entry in entries.filter_map(Result::ok) {
                let p = entry.path();
                if p.is_dir() {
                    let pkg_name = entry.file_name().to_string_lossy().to_string();
                    if pkg_name.starts_with('.') {
                        continue;
                    }
                    let mut ver = String::new();
                    if let Ok(pkg_json) = fs::read_to_string(p.join("package.json")) {
                        for line in pkg_json.lines() {
                            if line.trim().starts_with("\"version\":")
                                && let Some(v) = line.split('"').nth(3)
                            {
                                ver = format!("v{}", v);
                                break;
                            }
                        }
                    }
                    let size = get_dir_size(&p);
                    if size > 0 {
                        npm_total += size;
                        let label = if !ver.is_empty() {
                            format!("global: {} ({})", pkg_name, ver)
                        } else {
                            format!("global: {}", pkg_name)
                        };
                        npm_items.push(PackageStorageItem {
                            name: label,
                            detail: p.to_string_lossy().to_string(),
                            size_bytes: size,
                            size_str: format_bytes_dyn(size as f64),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }

    // Scan NVM versions
    let nvm_versions = format!("{}/.nvm/versions/node", home);
    if let Ok(entries) = fs::read_dir(&nvm_versions) {
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() {
                let ver_name = entry.file_name().to_string_lossy().to_string();
                let size = get_dir_size(&p);
                if size > 0 {
                    npm_total += size;
                    npm_items.push(PackageStorageItem {
                        name: format!("Node.js runtime ({})", ver_name),
                        detail: p.to_string_lossy().to_string(),
                        size_bytes: size,
                        size_str: format_bytes_dyn(size as f64),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // pnpm global & store
    let pnpm_store = format!("{}/.local/share/pnpm", home);
    if std::path::Path::new(&pnpm_store).exists() {
        let pnpm_size = get_dir_size(&pnpm_store);
        if pnpm_size > 0 {
            npm_total += pnpm_size;
            npm_items.push(PackageStorageItem {
                name: "pnpm Store (~/.local/share/pnpm)".to_string(),
                detail: "Content-addressable package store".to_string(),
                size_bytes: pnpm_size,
                size_str: format_bytes_dyn(pnpm_size as f64),
                ..Default::default()
            });
        }
    }

    // Yarn cache
    for y_path in &[
        format!("{}/.cache/yarn", home),
        format!("{}/.yarn/berry/cache", home),
    ] {
        if std::path::Path::new(y_path).exists() {
            let y_size = get_dir_size(y_path);
            if y_size > 0 {
                npm_total += y_size;
                npm_items.push(PackageStorageItem {
                    name: "Yarn Package Cache".to_string(),
                    detail: y_path.to_string(),
                    size_bytes: y_size,
                    size_str: format_bytes_dyn(y_size as f64),
                    ..Default::default()
                });
            }
        }
    }

    // Bun cache
    let bun_cache = format!("{}/.bun/install/cache", home);
    if std::path::Path::new(&bun_cache).exists() {
        let bun_size = get_dir_size(&bun_cache);
        if bun_size > 0 {
            npm_total += bun_size;
            npm_items.push(PackageStorageItem {
                name: "Bun Package Cache".to_string(),
                detail: bun_cache,
                size_bytes: bun_size,
                size_str: format_bytes_dyn(bun_size as f64),
                ..Default::default()
            });
        }
    }

    if npm_total > 0 || !npm_items.is_empty() {
        npm_items.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        categories.push(PackageStorageCategory {
            name: "npm".to_string(),
            total_str: format_bytes_dyn(npm_total as f64),
            items: npm_items,
        });
    }

    categories
}

/// Represents an active network socket connection (TCP / UDP) parsed from `/proc/net/`.
#[derive(Debug, Clone)]
pub struct NetConnectionInfo {
    /// Protocol type ("TCP", "TCP6", "UDP", "UDP6").
    pub proto: &'static str,
    /// Local bound IP address.
    pub local_ip: IpAddr,
    /// Local bound port number.
    pub local_port: u16,
    /// Resolved reverse DNS local hostname if available.
    pub local_host: Option<String>,
    /// Remote peer IP address.
    pub remote_ip: IpAddr,
    /// Remote peer port number.
    pub remote_port: u16,
    /// Connection state (e.g. "ESTABLISHED", "LISTEN", "TIME_WAIT").
    pub state: &'static str,
    /// Owning Process PID if resolvable.
    pub pid: Option<u32>,
    /// Owning Process name/command if resolvable.
    pub process_name: Option<String>,
    /// Resolved reverse DNS hostname if available.
    pub remote_host: Option<String>,
    /// Socket inode identifier.
    #[allow(dead_code)]
    pub inode: u64,
}

impl Default for NetConnectionInfo {
    fn default() -> Self {
        Self {
            proto: "TCP",
            local_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            local_port: 0,
            local_host: None,
            remote_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            remote_port: 0,
            state: "UNKNOWN",
            pid: None,
            process_name: None,
            remote_host: None,
            inode: 0,
        }
    }
}

/// Asynchronous non-blocking reverse DNS resolver with thread-safe cache and local host mapping.
pub struct DnsResolver {
    /// Static IP-to-hostname map loaded from `/etc/hostname`, `/etc/hosts`, and `/proc/net/fib_trie`.
    static_hosts: HashMap<IpAddr, String>,
    /// Thread-safe map of cached resolved hostnames.
    cache: Arc<Mutex<HashMap<IpAddr, Option<String>>>>,
    /// Channel sender for requesting async reverse DNS resolution.
    req_tx: Sender<IpAddr>,
}

impl Default for DnsResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsResolver {
    /// Creates a new background DNS resolver worker.
    pub fn new() -> Self {
        let static_hosts = get_local_hosts_map();
        let cache = Arc::new(Mutex::new(HashMap::new()));
        let (req_tx, req_rx): (Sender<IpAddr>, Receiver<IpAddr>) = channel();
        let cache_clone = Arc::clone(&cache);

        std::thread::Builder::new()
            .name("dns-resolver".to_string())
            .spawn(move || {
                let mut requested: HashSet<IpAddr> = HashSet::new();
                while let Ok(ip) = req_rx.recv() {
                    if requested.contains(&ip) {
                        continue;
                    }
                    requested.insert(ip);

                    if ip.is_loopback() || ip.is_unspecified() {
                        if let Ok(mut c) = cache_clone.lock() {
                            c.insert(ip, Some("localhost".to_string()));
                        }
                        continue;
                    }

                    let resolved = reverse_dns_lookup(ip);
                    if let Ok(mut c) = cache_clone.lock() {
                        c.insert(ip, resolved);
                    }
                }
            })
            .ok();

        Self {
            static_hosts,
            cache,
            req_tx,
        }
    }

    /// Queries the DNS cache for an IP hostname, scheduling async lookup if absent.
    pub fn get_or_resolve(&self, ip: IpAddr) -> Option<String> {
        if ip.is_loopback() || ip.is_unspecified() {
            return Some("localhost".to_string());
        }
        if let Some(host) = self.static_hosts.get(&ip) {
            return Some(host.clone());
        }
        if let Ok(c) = self.cache.lock()
            && let Some(cached) = c.get(&ip)
        {
            return cached.clone();
        }
        let _ = self.req_tx.send(ip);
        None
    }
}

/// Loads static IP-to-hostname mappings from `/etc/hostname`, `/etc/hosts`, and `/proc/net/fib_trie`.
fn get_local_hosts_map() -> HashMap<IpAddr, String> {
    let mut map = HashMap::new();

    map.insert(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        "localhost".to_string(),
    );
    map.insert(IpAddr::V6(Ipv6Addr::LOCALHOST), "localhost".to_string());

    let hostname = fs::read_to_string("/etc/hostname")
        .or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| {
            rustix::system::uname()
                .nodename()
                .to_string_lossy()
                .to_string()
        });

    if !hostname.is_empty() {
        map.insert(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), hostname.clone());

        // Parse local IPv4 addresses from /proc/net/fib_trie
        if let Ok(fib) = fs::read_to_string("/proc/net/fib_trie") {
            let mut prev_ip: Option<IpAddr> = None;
            for line in fib.lines() {
                if line.contains("|--") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    prev_ip = parts.last().and_then(|s| s.parse::<IpAddr>().ok());
                } else if line.contains("/32 host LOCAL")
                    && let Some(ip) = prev_ip
                {
                    map.insert(ip, hostname.clone());
                }
            }
        }

        // Parse local IPv6 addresses from /proc/net/if_inet6
        if let Ok(inet6) = fs::read_to_string("/proc/net/if_inet6") {
            for line in inet6.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(hex_ip) = parts.first()
                    && let Some(v6) = parse_ipv6_hex(hex_ip)
                {
                    map.insert(IpAddr::V6(v6), hostname.clone());
                }
            }
        }
    }

    if let Ok(hosts_content) = fs::read_to_string("/etc/hosts") {
        for line in hosts_content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            if let Some(ip_str) = parts.next()
                && let Some(host_str) = parts.next()
                && let Ok(ip) = ip_str.parse::<IpAddr>()
            {
                map.entry(ip).or_insert_with(|| host_str.to_string());
            }
        }
    }

    map
}

/// Performs safe reverse DNS lookup via system name service switch.
fn reverse_dns_lookup(ip: IpAddr) -> Option<String> {
    if ip.is_loopback() || ip.is_unspecified() {
        return Some("localhost".to_string());
    }

    let ip_str = ip.to_string();
    if let Ok(output) = std::process::Command::new("getent")
        .args(["hosts", &ip_str])
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = stdout.split_whitespace().collect();
        if parts.len() >= 2 {
            let host = parts[1];
            if !host.is_empty() && host != ip_str {
                return Some(host.to_string());
            }
        }
    }
    None
}

/// Builds a mapping of socket inode numbers to `(PID, process_comm)` by scanning `/proc/*/fd/`.
pub fn build_socket_inode_map() -> HashMap<u64, (u32, String)> {
    let mut map = HashMap::new();
    let Ok(proc_dir) = fs::read_dir("/proc") else {
        return map;
    };
    for entry in proc_dir.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        let Ok(pid) = name_str.parse::<u32>() else {
            continue;
        };

        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        let Ok(fd_dir) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd_entry in fd_dir.flatten() {
            if let Ok(link) = fs::read_link(fd_entry.path()) {
                let link_str = link.to_string_lossy();
                if let Some(rest) = link_str.strip_prefix("socket:[")
                    && let Some(inode_str) = rest.strip_suffix(']')
                    && let Ok(inode) = inode_str.parse::<u64>()
                {
                    map.insert(inode, (pid, comm.clone()));
                }
            }
        }
    }
    map
}

/// Parses a 32-bit IPv4 hex string in little-endian order into an `Ipv4Addr`.
fn parse_ipv4_hex(hex: &str) -> Option<Ipv4Addr> {
    let val = u32::from_str_radix(hex, 16).ok()?;
    Some(Ipv4Addr::from(val.to_le_bytes()))
}

/// Parses a 128-bit IPv6 hex string in 4 little-endian 32-bit words into an `Ipv6Addr`.
fn parse_ipv6_hex(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..4 {
        let word = u32::from_str_radix(&hex[i * 8..(i + 1) * 8], 16).ok()?;
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&word.to_le_bytes());
    }
    Some(Ipv6Addr::from(bytes))
}

/// Parses a 16-bit port hex string into a numeric port value.
fn parse_port_hex(hex: &str) -> Option<u16> {
    u16::from_str_radix(hex, 16).ok()
}

/// Translates Linux `/proc/net/tcp` hex connection state string to human-readable label.
fn parse_tcp_state(st_hex: &str) -> &'static str {
    match st_hex {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        _ => "UNKNOWN",
    }
}

/// Reads active network connections from `/proc/net/tcp*` and `/proc/net/udp*`.
///
/// # Returns
/// `Ok(Vec<NetConnectionInfo>)` or `Err("Root permissions required.")` if restricted.
pub fn read_network_connections(dns: &DnsResolver) -> Result<Vec<NetConnectionInfo>, &'static str> {
    let mut conns = Vec::new();
    let socket_map = build_socket_inode_map();

    let files = [
        ("/proc/net/tcp", "TCP", false),
        ("/proc/net/tcp6", "TCP6", true),
        ("/proc/net/udp", "UDP", false),
        ("/proc/net/udp6", "UDP6", true),
    ];

    let mut had_perm_error = false;
    let mut successfully_read = 0;

    for (path, proto, is_v6) in files {
        let content = match fs::read_to_string(path) {
            Ok(c) => {
                successfully_read += 1;
                c
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                had_perm_error = true;
                continue;
            }
            Err(_) => continue,
        };

        for line in content.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 10 {
                continue;
            }

            let local_parts: Vec<&str> = parts[1].split(':').collect();
            if local_parts.len() != 2 {
                continue;
            }
            let local_ip = if is_v6 {
                parse_ipv6_hex(local_parts[0])
                    .map(IpAddr::V6)
                    .unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
            } else {
                parse_ipv4_hex(local_parts[0])
                    .map(IpAddr::V4)
                    .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            };
            let local_port = parse_port_hex(local_parts[1]).unwrap_or(0);

            let rem_parts: Vec<&str> = parts[2].split(':').collect();
            if rem_parts.len() != 2 {
                continue;
            }
            let remote_ip = if is_v6 {
                parse_ipv6_hex(rem_parts[0])
                    .map(IpAddr::V6)
                    .unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
            } else {
                parse_ipv4_hex(rem_parts[0])
                    .map(IpAddr::V4)
                    .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            };
            let remote_port = parse_port_hex(rem_parts[1]).unwrap_or(0);

            let state = if proto.starts_with("UDP") {
                "UNCONN"
            } else {
                parse_tcp_state(parts[3])
            };
            let inode = parts[9].parse::<u64>().unwrap_or(0);

            let (pid, process_name) = if let Some((p, comm)) = socket_map.get(&inode) {
                (Some(*p), Some(comm.clone()))
            } else {
                (None, None)
            };

            let local_host = if !local_ip.is_unspecified() {
                dns.get_or_resolve(local_ip)
            } else {
                None
            };

            let remote_host = if !remote_ip.is_unspecified() {
                dns.get_or_resolve(remote_ip)
            } else {
                None
            };

            conns.push(NetConnectionInfo {
                proto,
                local_ip,
                local_port,
                local_host,
                remote_ip,
                remote_port,
                state,
                pid,
                process_name,
                remote_host,
                inode,
            });
        }
    }

    if successfully_read == 0 && had_perm_error {
        return Err("Root permissions required.");
    }

    // Sort: established connections first, then listening sockets
    conns.sort_by(|a, b| {
        let a_prio = match a.state {
            "ESTABLISHED" => 0,
            "SYN_SENT" | "SYN_RECV" => 1,
            "CLOSE_WAIT" | "TIME_WAIT" | "FIN_WAIT1" | "FIN_WAIT2" => 2,
            "LISTEN" => 3,
            _ => 4,
        };
        let b_prio = match b.state {
            "ESTABLISHED" => 0,
            "SYN_SENT" | "SYN_RECV" => 1,
            "CLOSE_WAIT" | "TIME_WAIT" | "FIN_WAIT1" | "FIN_WAIT2" => 2,
            "LISTEN" => 3,
            _ => 4,
        };
        a_prio.cmp(&b_prio).then_with(|| a.proto.cmp(b.proto))
    });

    Ok(conns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_gpu_metrics() {
        let metrics = read_gpu_metrics();
        println!("GPU Name: {}", metrics.name);
        println!("Driver: {}", metrics.driver);
        println!(
            "VRAM: {} MB / {} MB",
            metrics.vram_used_mb, metrics.vram_total_mb
        );
        println!("Utilization: {}%", metrics.utilization_pct);
        assert!(!metrics.name.is_empty());
    }

    #[test]
    fn test_gpu_data_collection_under_workload() {
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::{Duration, Instant};

        let baseline = read_gpu_metrics();
        println!("\n--- [GPU Test] Baseline ---");
        println!(
            "GPU: {} | VRAM: {}/{} MB | Utilization: {}% | Clock: {} MHz",
            baseline.name,
            baseline.vram_used_mb,
            baseline.vram_total_mb,
            baseline.utilization_pct,
            baseline.cur_mhz
        );

        // Run VAAPI hardware encode if ffmpeg is available
        if let Ok(mut child) = Command::new("ffmpeg")
            .args([
                "-init_hw_device",
                "vaapi=va:/dev/dri/renderD128",
                "-filter_hw_device",
                "va",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=3840x2160:rate=60",
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-b:v",
                "50M",
                "-t",
                "3",
                "-f",
                "null",
                "-",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let start = Instant::now();
            let mut samples = Vec::new();
            while start.elapsed() < Duration::from_secs(4) {
                let m = read_gpu_metrics();
                samples.push(m);
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                thread::sleep(Duration::from_millis(300));
            }
            let _ = child.wait();

            println!(
                "--- [GPU Test] Workload Samples ({} samples collected) ---",
                samples.len()
            );
            for (idx, s) in samples.iter().enumerate() {
                println!(
                    "  Sample {}: Util: {:>3.0}%, VRAM: {:>4} MB, Clock: {:>4.0} MHz, Power: {:>4.1} W, Temp: {:>2}°C",
                    idx + 1,
                    s.utilization_pct,
                    s.vram_used_mb,
                    s.cur_mhz,
                    s.power_w,
                    s.temp_edge_c
                );
            }
            assert!(!samples.is_empty());
        }
    }

    #[test]
    fn test_read_package_storage_categories() {
        let cats = read_package_storage_categories();
        // Check that any detected category has valid non-empty items and names
        for cat in &cats {
            assert!(!cat.name.is_empty());
            for item in &cat.items {
                assert!(!item.name.is_empty());
            }
        }
    }

    #[test]
    fn test_parse_dust_output() {
        let sample = "  0B   ┌── proc      │█                                                   │   0%\n\
  0B   ├── sys       │█                                                   │   0%\n\
4.0K   ├── boot      │█                                                   │   0%\n\
 23G   ├── var       │████                                                │   7%\n\
120G   ├── nix       │████████████████████                                │  38%\n\
169G   ├── home      │████████████████████████████                        │  54%\n\
313G ┌─┴ /           │███████████████████████████████████████████████████ │ 100%\n";

        let cat = parse_dust_output(sample).expect("should parse dust output");
        assert_eq!(cat.name, "All");
        assert_eq!(cat.total_str, "313.0 GB");
        assert_eq!(cat.items.len(), 6);
        assert_eq!(cat.items[0].name, "/home");
        assert_eq!(cat.items[0].size_str, "169.0 GB");
        assert_eq!(cat.items[0].detail, "54%");
        assert_eq!(cat.items[1].name, "/nix");
        assert_eq!(cat.items[1].size_str, "120.0 GB");
        assert_eq!(cat.items[1].detail, "38%");
        assert_eq!(cat.items[2].name, "/var");
        assert_eq!(cat.items[2].size_str, "23.0 GB");
        assert_eq!(cat.items[2].detail, "7%");
    }

    #[test]
    fn test_parse_dust_children() {
        let sample = r#"
4.0K   ┌── slop │█                                                        │   0%
4.0K   ├── work │█                                                        │   0%
169G   ├── dazor│████████████████████████████████████████████████████████ │ 100%
169G ┌─┴ home   │████████████████████████████████████████████████████████ │ 100%
"#;
        let children = parse_dust_children("/home", 0, sample);
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].name, "dazor");
        assert_eq!(children[0].path, "/home/dazor");
        assert_eq!(children[0].depth, 1);
        assert_eq!(children[0].size_str, "169.0 GB");
        assert_eq!(children[0].detail, "100%");
        assert_eq!(children[1].name, "slop");
        assert_eq!(children[1].path, "/home/slop");
        assert_eq!(children[1].detail, "0%");
        assert_eq!(children[2].name, "work");
        assert_eq!(children[2].path, "/home/work");
        assert_eq!(children[2].detail, "0%");
    }

    #[test]
    fn test_is_item_visible() {
        let visible_item = PackageStorageItem {
            name: "Documents".to_string(),
            detail: String::new(),
            size_bytes: 50_000,
            size_str: "50.0 KB".to_string(),
            path: "/home/user/Documents".to_string(),
            is_dir: true,
            is_expanded: false,
            is_scanning: false,
            depth: 1,
        };
        let four_kb_folder = PackageStorageItem {
            name: "/root".to_string(),
            detail: String::new(),
            size_bytes: 4096,
            size_str: "4.0 KB".to_string(),
            path: "/root".to_string(),
            is_dir: true,
            is_expanded: false,
            is_scanning: false,
            depth: 0,
        };
        let hidden_item = PackageStorageItem {
            name: ".cache".to_string(),
            detail: String::new(),
            size_bytes: 10_000_000,
            size_str: "10 MB".to_string(),
            path: "/home/user/.cache".to_string(),
            is_dir: true,
            is_expanded: false,
            is_scanning: false,
            depth: 1,
        };
        let small_item = PackageStorageItem {
            name: "tiny.txt".to_string(),
            detail: String::new(),
            size_bytes: 1024,
            size_str: "1.0 KB".to_string(),
            path: "/home/user/tiny.txt".to_string(),
            is_dir: false,
            is_expanded: false,
            is_scanning: false,
            depth: 1,
        };
        let root_dot_item = PackageStorageItem {
            name: "/.snapshots".to_string(),
            detail: String::new(),
            size_bytes: 50_000,
            size_str: "50 KB".to_string(),
            path: "/.snapshots".to_string(),
            is_dir: true,
            is_expanded: false,
            is_scanning: false,
            depth: 0,
        };

        let hidden_large_item = PackageStorageItem {
            name: ".local".to_string(),
            detail: String::new(),
            size_bytes: 5 * 1024 * 1024 * 1024,
            size_str: "5.0 GB".to_string(),
            path: "/home/user/.local".to_string(),
            is_dir: true,
            is_expanded: false,
            is_scanning: false,
            depth: 1,
        };

        // When show_hidden = false (default)
        assert!(is_item_visible(&visible_item, false));
        assert!(!is_item_visible(&four_kb_folder, false));
        assert!(!is_item_visible(&hidden_item, false));
        assert!(!is_item_visible(&small_item, false));
        assert!(!is_item_visible(&root_dot_item, false));
        // Edge case: hidden but >= 1GB is visible by default
        assert!(is_item_visible(&hidden_large_item, false));

        // When show_hidden = true
        assert!(is_item_visible(&visible_item, true));
        assert!(is_item_visible(&four_kb_folder, true));
        assert!(is_item_visible(&hidden_item, true));
        assert!(is_item_visible(&small_item, true));
        assert!(is_item_visible(&root_dot_item, true));
        assert!(is_item_visible(&hidden_large_item, true));
    }

    #[test]
    fn test_format_power_plan() {
        assert_eq!(format_power_plan("performance"), "Performance");
        assert_eq!(format_power_plan("balanced"), "Balanced");
        assert_eq!(
            format_power_plan("balance_performance"),
            "Balanced Performance"
        );
        assert_eq!(
            format_power_plan("balanced-performance"),
            "Balanced Performance"
        );
        assert_eq!(format_power_plan("balance_power"), "Balanced Power");
        assert_eq!(format_power_plan("powersave"), "Power Saver");
        assert_eq!(format_power_plan("power-saver"), "Power Saver");
        assert_eq!(format_power_plan("low-power"), "Power Saver");
        assert_eq!(format_power_plan("quiet"), "Quiet");
        assert_eq!(format_power_plan("cool"), "Cool");
        assert_eq!(format_power_plan("schedutil"), "Schedutil");
        assert_eq!(format_power_plan("ondemand"), "Ondemand");
        assert_eq!(format_power_plan("conservative"), "Conservative");
        assert_eq!(format_power_plan("userspace"), "Userspace");
        assert_eq!(
            format_power_plan("extreme_performance"),
            "Extreme Performance"
        );
        assert_eq!(format_power_plan(""), "");
    }

    #[test]
    fn test_read_power_plan() {
        let plan = read_power_plan();
        println!("Detected power plan: {:?}", plan);
    }

    #[test]
    fn test_detect_ram_by_cpu() {
        // AMD Ryzen 7000 / 8000 / 9000 / AI 300 (DDR5 5600MHz)
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 7840HS with Radeon 780M Graphics"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 7640HS w/ Radeon 760M Graphics"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 8845HS w/ Radeon 780M Graphics"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 8645HS"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 9 8945HS"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen AI 9 HX 370"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 7800X3D 8-Core Processor"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 7600X 6-Core Processor"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 7500F 6-Core Processor"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 9700X 8-Core Processor"),
            Some(("DDR5", "5600MHz"))
        );

        // AMD Ryzen 6000 & 7035 series (DDR5 4800MHz)
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 6800H with Radeon Graphics"),
            Some(("DDR5", "4800MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 6600H"),
            Some(("DDR5", "4800MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 7735HS with Radeon Graphics"),
            Some(("DDR5", "4800MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 7535HS with Radeon Graphics"),
            Some(("DDR5", "4800MHz"))
        );

        // AMD Ryzen 5000 / 3000 (DDR4 3200MHz)
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 7 5800X 8-Core Processor"),
            Some(("DDR4", "3200MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("AMD Ryzen 5 3600X 6-Core Processor"),
            Some(("DDR4", "3200MHz"))
        );

        // Intel 13th & 14th Gen without "13th"/"14th" in string (DDR5 5600MHz)
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-13700H"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-13700HX"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-13620H"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i5-13500H"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i9-13900HX"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("13th Gen Intel(R) Core(TM) i7-13700H"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-14700HX"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i9-14900HX"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i5-14500HX"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("14th Gen Intel(R) Core(TM) i7-14700HX"),
            Some(("DDR5", "5600MHz"))
        );

        // Intel Core Ultra (DDR5 5600MHz)
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) Ultra 7 155H"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) Ultra 5 125H"),
            Some(("DDR5", "5600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) Ultra 7 258V"),
            Some(("DDR5", "5600MHz"))
        );

        // Intel 12th Gen (DDR5 4800MHz for H/HX)
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-12700H"),
            Some(("DDR5", "4800MHz"))
        );

        // Legacy Intel (DDR4 / DDR3)
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-11800H"),
            Some(("DDR4", "3200MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-10750H"),
            Some(("DDR4", "3200MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i7-4770K"),
            Some(("DDR3", "1600MHz"))
        );
        assert_eq!(
            detect_ram_by_cpu("Intel(R) Core(TM) i5-2500K"),
            Some(("DDR3", "1600MHz"))
        );

        // Apple Silicon
        assert_eq!(detect_ram_by_cpu("Apple M1"), Some(("LPDDR4X", "4266MHz")));
        assert_eq!(
            detect_ram_by_cpu("Apple M2 Max"),
            Some(("LPDDR5", "6400MHz"))
        );

        // Unknown / Virtual CPU without match
        assert_eq!(detect_ram_by_cpu("CPU"), None);
        assert_eq!(detect_ram_by_cpu("Common KVM processor"), None);
        assert_eq!(detect_ram_by_cpu("QEMU Virtual CPU"), None);
    }

    #[test]
    fn test_get_ram_info() {
        let ram_str = get_ram_info(32768, "AMD Ryzen 7 7840HS with Radeon 780M Graphics");
        assert!(ram_str.contains("32GB"));
        assert!(ram_str.contains("DDR5"));
        assert!(ram_str.contains("5600MHz"));

        let unknown_ram = get_ram_info(16384, "Common KVM processor");
        assert_eq!(unknown_ram, "16GB [root permissions required]");
    }

    #[test]
    fn test_read_ram_temp() {
        let temp = read_ram_temp();
        println!("Detected RAM temperature: {:?}", temp);
    }
}
