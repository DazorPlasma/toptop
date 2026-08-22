//! Point-in-time telemetry snapshots and fixed-length metric history buffers.
//!
//! A [`Snapshot`] captures every reading the monitor displays for one polling
//! interval. The application keeps a live snapshot that is updated in place,
//! and a ring of historical snapshots enabling time-travel inspection.

use crate::{
    process::ProcessInfo,
    system::{
        BatteryInfo, DiskIoInfo, GpuMetrics, MemoryMetrics, MountInfo, NetConnectionInfo,
        NetInterfaceInfo, SystemGeneralInfo,
    },
};

/// Number of samples retained per trend graph (one sample per poll tick).
pub const HISTORY_LEN: usize = 100;

/// One complete set of instantaneous readings pushed onto [`MetricHistory`].
#[derive(Clone, Copy, Debug, Default)]
pub struct MetricSample {
    /// Overall CPU busy percentage.
    pub cpu: f64,
    /// RAM usage percentage.
    pub mem: f64,
    /// Swap usage percentage.
    pub swap: f64,
    /// GPU core busy percentage.
    pub gpu: f64,
    /// GPU VRAM usage percentage.
    pub gpu_vram: f64,
    /// Primary interface receive throughput gradient percentage.
    pub net_rx: f64,
    /// Primary interface transmit throughput gradient percentage.
    pub net_tx: f64,
    /// Disk read throughput gradient percentage.
    pub disk_read: f64,
    /// Disk write throughput gradient percentage.
    pub disk_write: f64,
}

/// Fixed-length ring buffers of historical samples feeding the Braille trend graphs.
///
/// Every series holds exactly [`HISTORY_LEN`] entries; `shift()` slides the
/// window left by one and [`MetricHistory::push`](Self::push) writes the newest
/// sample into the final slot.
#[derive(Clone, Debug)]
pub struct MetricHistory {
    /// Historical CPU percentages.
    pub cpu: Vec<Option<f64>>,
    /// Historical RAM percentages.
    pub mem: Vec<Option<f64>>,
    /// Historical swap percentages.
    pub swap: Vec<Option<f64>>,
    /// Historical GPU core percentages.
    pub gpu: Vec<Option<f64>>,
    /// Historical GPU VRAM percentages.
    pub gpu_vram: Vec<Option<f64>>,
    /// Historical network receive rates.
    pub net_rx: Vec<Option<f64>>,
    /// Historical network transmit rates.
    pub net_tx: Vec<Option<f64>>,
    /// Historical disk read rates.
    pub disk_read: Vec<Option<f64>>,
    /// Historical disk write rates.
    pub disk_write: Vec<Option<f64>>,
}

impl Default for MetricHistory {
    fn default() -> Self {
        let blank = || vec![None; HISTORY_LEN];
        Self {
            cpu: blank(),
            mem: blank(),
            swap: blank(),
            gpu: blank(),
            gpu_vram: blank(),
            net_rx: blank(),
            net_tx: blank(),
            disk_read: blank(),
            disk_write: blank(),
        }
    }
}

impl MetricHistory {
    /// Slides every series left by one slot, dropping the oldest sample.
    pub fn shift(&mut self) {
        self.cpu.copy_within(1.., 0);
        self.mem.copy_within(1.., 0);
        self.swap.copy_within(1.., 0);
        self.gpu.copy_within(1.., 0);
        self.gpu_vram.copy_within(1.., 0);
        self.net_rx.copy_within(1.., 0);
        self.net_tx.copy_within(1.., 0);
        self.disk_read.copy_within(1.., 0);
        self.disk_write.copy_within(1.., 0);
    }

    /// Writes a fresh sample into the newest slot of every series.
    pub fn push(&mut self, sample: MetricSample) {
        let last = HISTORY_LEN - 1;
        self.cpu[last] = Some(sample.cpu);
        self.mem[last] = Some(sample.mem);
        self.swap[last] = Some(sample.swap);
        self.gpu[last] = Some(sample.gpu);
        self.gpu_vram[last] = Some(sample.gpu_vram);
        self.net_rx[last] = Some(sample.net_rx);
        self.net_tx[last] = Some(sample.net_tx);
        self.disk_read[last] = Some(sample.disk_read);
        self.disk_write[last] = Some(sample.disk_write);
    }
}

/// Complete system telemetry for a single poll interval.
///
/// The application mutates one "live" instance in place each tick; pushing a
/// clone onto the history ring freezes it for time-travel inspection.
#[derive(Clone)]
pub struct Snapshot {
    /// System overview metadata.
    pub sys_info: SystemGeneralInfo,
    /// Battery telemetry status.
    pub battery: Option<BatteryInfo>,
    /// Overall CPU busy percentage.
    pub global_usage: f64,
    /// Per-core CPU busy percentages.
    pub core_usages: Vec<f64>,
    /// Current CPU clock frequency in MHz.
    pub cpu_cur_mhz: f64,
    /// Minimum CPU clock frequency in MHz.
    pub cpu_min_mhz: f64,
    /// Maximum CPU clock frequency in MHz.
    pub cpu_max_mhz: f64,
    /// Package CPU temperature in degrees Celsius.
    pub cpu_temp: u32,
    /// System RAM / DIMM temperature in degrees Celsius if sensor is available.
    pub ram_temp: Option<u32>,
    /// Memory metrics.
    pub mem: MemoryMetrics,
    /// Formatted RAM capacity and speed info string.
    pub ram_info: String,
    /// GPU telemetry and clock/power metrics.
    pub gpu_metrics: GpuMetrics,
    /// Active network interfaces and throughput rates.
    pub net_ifaces: Vec<NetInterfaceInfo>,
    /// Active socket connections.
    pub net_connections: Result<Vec<NetConnectionInfo>, &'static str>,
    /// Disk I/O throughput rates.
    pub disk_io: DiskIoInfo,
    /// Mounted partition usage.
    pub disk_mounts: Vec<MountInfo>,
    /// Process list snapshot.
    pub processes: Vec<ProcessInfo>,
    /// Historical metric samples for the Braille trend graphs.
    pub history: MetricHistory,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            sys_info: Default::default(),
            battery: None,
            global_usage: 0.0,
            core_usages: Vec::new(),
            cpu_cur_mhz: 0.0,
            cpu_min_mhz: 0.0,
            cpu_max_mhz: 0.0,
            cpu_temp: 0,
            ram_temp: None,
            mem: Default::default(),
            ram_info: String::new(),
            gpu_metrics: Default::default(),
            net_ifaces: Vec::new(),
            net_connections: Ok(Vec::new()),
            disk_io: Default::default(),
            disk_mounts: Vec::new(),
            processes: Vec::new(),
            history: Default::default(),
        }
    }
}

impl Snapshot {
    /// RAM usage as a percentage of installed memory.
    pub fn mem_used_pct(&self) -> f64 {
        used_pct(self.mem.used_mem_mb, self.mem.total_mem_mb)
    }

    /// Swap usage as a percentage of configured swap space.
    pub fn swap_used_pct(&self) -> f64 {
        used_pct(self.mem.used_swap_mb, self.mem.total_swap_mb)
    }

    /// Dedicated VRAM usage as a percentage of total VRAM.
    pub fn vram_used_pct(&self) -> f64 {
        used_pct(
            self.gpu_metrics.vram_used_mb,
            self.gpu_metrics.vram_total_mb,
        )
    }
}

/// Computes a saturation-safe usage percentage from byte/MB counters.
fn used_pct(used: u64, total: u64) -> f64 {
    if total > 0 {
        (used as f64 / total as f64) * 100.0
    } else {
        0.0
    }
}
