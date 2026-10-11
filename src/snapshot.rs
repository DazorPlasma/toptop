//! Point-in-time telemetry snapshots and fixed-length metric history buffers.
//!
//! A [`Snapshot`] captures every reading the monitor displays for one polling
//! interval. The application keeps a live snapshot that is updated in place,
//! and a ring of historical snapshots enabling time-travel inspection.

use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
};

use crate::{
    process::{ArchiveReader, ArchiveStringTable, ArchiveWriter, ArchivedProcessList, ProcessInfo},
    process_network::NetworkSource,
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
    pub gpu: Option<f64>,
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

/// One immutable chart point; `None` preserves uncollected startup values.
#[derive(Debug)]
struct MetricPoint {
    values: [Option<f64>; 9],
}

#[derive(Clone, Copy)]
pub enum MetricKind {
    Cpu = 0,
    Mem = 1,
    Swap = 2,
    Gpu = 3,
    GpuVram = 4,
    NetRx = 5,
    NetTx = 6,
    DiskRead = 7,
    DiskWrite = 8,
}

/// Shared immutable chart points; snapshots copy pointers rather than sample values.
#[derive(Clone, Debug, Default)]
pub struct MetricHistory {
    points: VecDeque<Arc<MetricPoint>>,
}

impl MetricHistory {
    /// The startup frame had only these five metrics available.
    pub fn seed(&mut self, cpu: f64, mem: f64, swap: f64, gpu: Option<f64>, gpu_vram: f64) {
        self.points.push_back(Arc::new(MetricPoint {
            values: [
                Some(cpu),
                Some(mem),
                Some(swap),
                gpu,
                Some(gpu_vram),
                None,
                None,
                None,
                None,
            ],
        }));
    }

    /// Appends one sample and retains exactly the latest 100 points.
    pub fn push(&mut self, sample: MetricSample) {
        if self.points.len() == HISTORY_LEN {
            self.points.pop_front();
        }
        self.points.push_back(Arc::new(MetricPoint {
            values: [
                Some(sample.cpu),
                Some(sample.mem),
                Some(sample.swap),
                sample.gpu,
                Some(sample.gpu_vram),
                Some(sample.net_rx),
                Some(sample.net_tx),
                Some(sample.disk_read),
                Some(sample.disk_write),
            ],
        }));
    }

    /// Rebuilds one chart series with the same blank prefix as the original ring.
    pub fn series(&self, metric: MetricKind) -> [Option<f64>; HISTORY_LEN] {
        let mut values = [None; HISTORY_LEN];
        for (index, point) in self.points.iter().enumerate() {
            values[HISTORY_LEN - self.points.len() + index] = point.values[metric as usize];
        }
        values
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
    /// Coverage of the per-process network rates in this snapshot.
    pub process_net_source: NetworkSource,
    /// Historical metric samples for the Braille trend graphs.
    pub history: MetricHistory,
    /// Compressed process and connection history while this snapshot is inactive.
    pub(crate) archived_history: Option<ArchivedHistory>,
}

#[derive(Clone, Debug)]
pub(crate) struct ArchivedHistory {
    processes: ArchivedProcessList,
    connections: ArchivedConnections,
}

#[derive(Clone, Debug)]
enum ArchivedConnections {
    Values {
        bytes: Arc<[u8]>,
        labels: Arc<[&'static str]>,
    },
    Error(&'static str),
}

#[derive(Default)]
struct ConnectionLabelTable {
    ids: std::collections::HashMap<&'static str, usize>,
    values: Vec<&'static str>,
}

impl ConnectionLabelTable {
    fn intern(&mut self, value: &'static str) -> usize {
        if let Some(&id) = self.ids.get(value) {
            return id;
        }
        let id = self.values.len();
        self.values.push(value);
        self.ids.insert(value, id);
        id
    }

    fn id(&self, value: &'static str) -> usize {
        *self
            .ids
            .get(value)
            .expect("connection label is absent from archive table")
    }
}

impl ArchivedConnections {
    fn encode(connections: &Result<Vec<NetConnectionInfo>, &'static str>) -> Self {
        let Ok(connections) = connections else {
            return Self::Error(*connections.as_ref().unwrap_err());
        };

        let mut strings = ArchiveStringTable::new();
        let mut labels = ConnectionLabelTable::default();
        for connection in connections {
            labels.intern(connection.proto);
            labels.intern(connection.state);
            for value in [
                connection.local_host.as_deref(),
                connection.process_name.as_deref(),
                connection.remote_host.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                strings.intern(value);
            }
        }

        let mut out = ArchiveWriter::with_capacity(connections.len().saturating_mul(48));
        out.byte(1); // Archive format version.
        out.varint(strings.values().len() as u64);
        for value in strings.values() {
            out.string(value);
        }
        out.varint(connections.len() as u64);
        for connection in connections {
            out.string_id(labels.id(connection.proto));
            write_ip(&mut out, connection.local_ip);
            out.u16(connection.local_port);
            write_optional_string_id(&mut out, &strings, connection.local_host.as_deref());
            write_ip(&mut out, connection.remote_ip);
            out.u16(connection.remote_port);
            out.string_id(labels.id(connection.state));
            if let Some(pid) = connection.pid {
                out.byte(1);
                out.varint(u64::from(pid));
            } else {
                out.byte(0);
            }
            write_optional_string_id(&mut out, &strings, connection.process_name.as_deref());
            write_optional_string_id(&mut out, &strings, connection.remote_host.as_deref());
        }
        Self::Values {
            bytes: out.finish(),
            labels: Arc::from(labels.values),
        }
    }

    fn materialize(&self) -> Result<Vec<NetConnectionInfo>, &'static str> {
        let Self::Values { bytes, labels } = self else {
            let Self::Error(message) = self else {
                unreachable!();
            };
            return Err(message);
        };

        let mut input = ArchiveReader::new(bytes);
        assert_eq!(
            input.byte(),
            1,
            "unsupported connection history archive version"
        );
        let string_count = input.usize();
        let strings: Vec<Arc<str>> = (0..string_count)
            .map(|_| Arc::<str>::from(input.string()))
            .collect();
        let connection_count = input.usize();
        let mut connections = Vec::with_capacity(connection_count);
        for _ in 0..connection_count {
            let proto = labels[input.string_id(labels.len())];
            let local_ip = read_ip(&mut input);
            let local_port = input.u16();
            let local_host = read_optional_string(&mut input, &strings);
            let remote_ip = read_ip(&mut input);
            let remote_port = input.u16();
            let state = labels[input.string_id(labels.len())];
            let pid = if input.byte() != 0 {
                Some(u32::try_from(input.varint()).expect("invalid PID in connection archive"))
            } else {
                None
            };
            let process_name = read_optional_string(&mut input, &strings);
            let remote_host = read_optional_string(&mut input, &strings);
            connections.push(NetConnectionInfo {
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
            });
        }
        Ok(connections)
    }
}

fn write_ip(out: &mut ArchiveWriter, ip: IpAddr) {
    match ip {
        IpAddr::V4(ip) => {
            out.byte(4);
            out.u32(u32::from_be_bytes(ip.octets()));
        }
        IpAddr::V6(ip) => {
            out.byte(6);
            let octets = ip.octets();
            out.u64(u64::from_be_bytes(
                octets[..8].try_into().expect("IPv6 half"),
            ));
            out.u64(u64::from_be_bytes(
                octets[8..].try_into().expect("IPv6 half"),
            ));
        }
    }
}

fn read_ip(input: &mut ArchiveReader<'_>) -> IpAddr {
    match input.byte() {
        4 => IpAddr::V4(Ipv4Addr::from(input.u32().to_be_bytes())),
        6 => {
            let mut octets = [0u8; 16];
            octets[..8].copy_from_slice(&input.u64().to_be_bytes());
            octets[8..].copy_from_slice(&input.u64().to_be_bytes());
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => panic!("invalid IP address family in history archive"),
    }
}

fn write_optional_string_id(
    out: &mut ArchiveWriter,
    strings: &ArchiveStringTable<'_>,
    value: Option<&str>,
) {
    if let Some(value) = value {
        out.byte(1);
        out.string_id(strings.id(value));
    } else {
        out.byte(0);
    }
}

fn read_optional_string(input: &mut ArchiveReader<'_>, strings: &[Arc<str>]) -> Option<String> {
    (input.byte() != 0).then(|| strings[input.string_id(strings.len())].to_string())
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
            process_net_source: NetworkSource::Unavailable,
            history: Default::default(),
            archived_history: None,
        }
    }
}

impl Snapshot {
    /// Creates a historical snapshot and immediately compacts its large row lists.
    ///
    /// Every collected process and connection field is retained. The archive
    /// stores exact floating point bit patterns and deduplicates strings within
    /// each sample; inactive history frames therefore avoid per-row Vec, String,
    /// and Option allocation overhead.
    pub fn historical_copy(&self) -> Self {
        let archived_history = self
            .archived_history
            .clone()
            .unwrap_or_else(|| ArchivedHistory {
                processes: ArchivedProcessList::encode(&self.processes),
                connections: ArchivedConnections::encode(&self.net_connections),
            });
        Self {
            sys_info: self.sys_info.clone(),
            battery: self.battery.clone(),
            global_usage: self.global_usage,
            core_usages: self.core_usages.clone(),
            cpu_cur_mhz: self.cpu_cur_mhz,
            cpu_min_mhz: self.cpu_min_mhz,
            cpu_max_mhz: self.cpu_max_mhz,
            cpu_temp: self.cpu_temp,
            ram_temp: self.ram_temp,
            mem: self.mem,
            ram_info: self.ram_info.clone(),
            gpu_metrics: self.gpu_metrics.clone(),
            net_ifaces: self.net_ifaces.clone(),
            net_connections: Ok(Vec::new()),
            disk_io: self.disk_io.clone(),
            disk_mounts: self.disk_mounts.clone(),
            processes: Vec::new(),
            process_net_source: self.process_net_source,
            history: self.history.clone(),
            archived_history: Some(archived_history),
        }
    }

    /// Packs process and connection rows, releasing their expanded allocations.
    /// Calling this repeatedly is safe and leaves already archived rows intact.
    pub fn compact_history(&mut self) {
        if self.archived_history.is_some() {
            return;
        }
        let archived_history = ArchivedHistory {
            processes: ArchivedProcessList::encode(&self.processes),
            connections: ArchivedConnections::encode(&self.net_connections),
        };
        self.processes = Vec::new();
        self.net_connections = Ok(Vec::new());
        self.archived_history = Some(archived_history);
    }

    /// Expands archived process and connection rows for the currently viewed frame.
    /// Calling this repeatedly is safe; a later [`Self::compact_history`] restores
    /// the same data losslessly.
    pub fn materialize_history(&mut self) {
        if let Some(archived) = self.archived_history.take() {
            self.processes = archived.processes.materialize();
            self.net_connections = archived.connections.materialize();
        }
    }

    /// Whether process and connection rows are currently stored in compact form.
    pub fn history_is_compact(&self) -> bool {
        self.archived_history.is_some()
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::GpuProcessSample;
    use std::collections::HashMap;

    fn sample(cpu: f64, gpu: Option<f64>) -> MetricSample {
        MetricSample {
            cpu,
            mem: cpu + 1.0,
            swap: cpu + 2.0,
            gpu,
            gpu_vram: cpu + 3.0,
            net_rx: cpu + 4.0,
            net_tx: cpu + 5.0,
            disk_read: cpu + 6.0,
            disk_write: cpu + 7.0,
        }
    }

    #[test]
    fn metric_history_seed_keeps_blank_prefix_and_unavailable_gpu() {
        let mut history = MetricHistory::default();
        history.seed(10.0, 20.0, 30.0, None, 40.0);

        let cpu = history.series(MetricKind::Cpu);
        assert!(cpu[..HISTORY_LEN - 1].iter().all(Option::is_none));
        assert_eq!(cpu[HISTORY_LEN - 1], Some(10.0));
        assert_eq!(history.series(MetricKind::Gpu)[HISTORY_LEN - 1], None);
        assert!(
            history
                .series(MetricKind::NetRx)
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn metric_history_rollover_retains_the_latest_hundred_points() {
        let mut history = MetricHistory::default();
        history.seed(0.0, 1.0, 2.0, None, 3.0);

        for value in 1..=HISTORY_LEN {
            history.push(sample(
                value as f64,
                (value % 2 == 0).then_some(value as f64),
            ));
        }

        let cpu = history.series(MetricKind::Cpu);
        assert_eq!(cpu[0], Some(1.0));
        assert_eq!(cpu[HISTORY_LEN - 1], Some(HISTORY_LEN as f64));
        assert!(cpu.iter().all(Option::is_some));

        let gpu = history.series(MetricKind::Gpu);
        assert_eq!(gpu[0], None);
        assert_eq!(gpu[1], Some(2.0));
        assert_eq!(gpu[HISTORY_LEN - 1], Some(HISTORY_LEN as f64));

        history.push(sample((HISTORY_LEN + 1) as f64, None));
        let cpu = history.series(MetricKind::Cpu);
        assert_eq!(cpu[0], Some(2.0));
        assert_eq!(cpu[HISTORY_LEN - 1], Some((HISTORY_LEN + 1) as f64));
        let gpu = history.series(MetricKind::Gpu);
        assert_eq!(gpu[0], Some(2.0));
        assert_eq!(gpu[HISTORY_LEN - 1], None);
    }

    #[test]
    fn cloned_metric_history_stays_frozen_when_live_history_advances() {
        let mut live = MetricHistory::default();
        live.seed(7.0, 8.0, 9.0, None, 10.0);
        live.push(sample(11.0, Some(12.0)));
        let frozen = live.clone();

        live.push(sample(13.0, None));

        let frozen_cpu = frozen.series(MetricKind::Cpu);
        assert_eq!(frozen_cpu[HISTORY_LEN - 2], Some(7.0));
        assert_eq!(frozen_cpu[HISTORY_LEN - 1], Some(11.0));
        let live_cpu = live.series(MetricKind::Cpu);
        assert_eq!(live_cpu[HISTORY_LEN - 2], Some(11.0));
        assert_eq!(live_cpu[HISTORY_LEN - 1], Some(13.0));

        assert_eq!(frozen.series(MetricKind::Gpu)[HISTORY_LEN - 1], Some(12.0));
        assert_eq!(live.series(MetricKind::Gpu)[HISTORY_LEN - 1], None);
    }

    fn process_fixture() -> ProcessInfo {
        let mut gpu_sample = GpuProcessSample::default();
        gpu_sample.engine_ns.insert("gfx".to_string(), u64::MAX - 2);
        gpu_sample.engine_capacity.insert("gfx".to_string(), 17);
        gpu_sample
            .engine_percent
            .insert("gfx".to_string(), f64::from_bits(0x7ff8_0000_0000_1234));
        gpu_sample.counter_regressed = true;
        ProcessInfo {
            pid: u32::MAX,
            ppid: 42,
            comm: Arc::from("browser-worker"),
            name: Arc::from("browser --profile /tmp/a"),
            state: "S (sleeping)".to_string(),
            rss_kb: 12_345,
            starttime_ticks: u64::MAX - 1,
            utime: 567,
            stime: 890,
            read_bytes: u64::MAX,
            write_bytes: 1 << 40,
            threads: 123,
            uid: 1000,
            user: Arc::from("dazor"),
            cpu_percent: f64::from_bits(0x7ff8_0000_0000_4321),
            read_speed: -0.0,
            write_speed: 1.25,
            net_rx_speed: Some(f64::INFINITY),
            net_tx_speed: None,
            net_partial: true,
            gpu_percent: Some(f64::from_bits(0x8000_0000_0000_0000)),
            gpu_sample: Some(Box::new(gpu_sample)),
            gpu_mem_kb: Some(u64::MAX - 3),
            pss_kb: None,
            grouped_pids: vec![1, 42, u32::MAX],
            is_group_header: true,
            is_group_child: false,
            group_name: Some("browser family".to_string()),
        }
    }

    fn assert_f64_eq_bits(left: f64, right: f64) {
        assert_eq!(left.to_bits(), right.to_bits());
    }

    fn assert_optional_f64_eq_bits(left: Option<f64>, right: Option<f64>) {
        match (left, right) {
            (Some(left), Some(right)) => assert_f64_eq_bits(left, right),
            (None, None) => {}
            pair => panic!("optional float mismatch: {pair:?}"),
        }
    }

    fn assert_f64_map_eq_bits(left: &HashMap<String, f64>, right: &HashMap<String, f64>) {
        assert_eq!(left.len(), right.len());
        for (key, left_value) in left {
            assert_f64_eq_bits(*left_value, right[key]);
        }
    }

    fn assert_processes_equal(left: &[ProcessInfo], right: &[ProcessInfo]) {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            assert_eq!(left.pid, right.pid);
            assert_eq!(left.ppid, right.ppid);
            assert_eq!(left.comm, right.comm);
            assert_eq!(left.name, right.name);
            assert_eq!(left.state, right.state);
            assert_eq!(left.rss_kb, right.rss_kb);
            assert_eq!(left.starttime_ticks, right.starttime_ticks);
            assert_eq!(left.utime, right.utime);
            assert_eq!(left.stime, right.stime);
            assert_eq!(left.read_bytes, right.read_bytes);
            assert_eq!(left.write_bytes, right.write_bytes);
            assert_eq!(left.threads, right.threads);
            assert_eq!(left.uid, right.uid);
            assert_eq!(left.user, right.user);
            assert_f64_eq_bits(left.cpu_percent, right.cpu_percent);
            assert_f64_eq_bits(left.read_speed, right.read_speed);
            assert_f64_eq_bits(left.write_speed, right.write_speed);
            assert_optional_f64_eq_bits(left.net_rx_speed, right.net_rx_speed);
            assert_optional_f64_eq_bits(left.net_tx_speed, right.net_tx_speed);
            assert_eq!(left.net_partial, right.net_partial);
            assert_optional_f64_eq_bits(left.gpu_percent, right.gpu_percent);
            match (&left.gpu_sample, &right.gpu_sample) {
                (Some(left), Some(right)) => {
                    assert_eq!(left.engine_ns, right.engine_ns);
                    assert_eq!(left.engine_capacity, right.engine_capacity);
                    assert_f64_map_eq_bits(&left.engine_percent, &right.engine_percent);
                    assert_eq!(left.counter_regressed, right.counter_regressed);
                }
                (None, None) => {}
                _ => panic!("GPU sample mismatch"),
            }
            assert_eq!(left.gpu_mem_kb, right.gpu_mem_kb);
            assert_eq!(left.pss_kb, right.pss_kb);
            assert_eq!(left.grouped_pids, right.grouped_pids);
            assert_eq!(left.is_group_header, right.is_group_header);
            assert_eq!(left.is_group_child, right.is_group_child);
            assert_eq!(left.group_name, right.group_name);
        }
    }

    fn connection_fixture() -> Vec<NetConnectionInfo> {
        vec![
            NetConnectionInfo {
                proto: "TCP6",
                local_ip: "2001:db8::abcd".parse().unwrap(),
                local_port: 443,
                local_host: Some("localhost-v6".to_string()),
                remote_ip: "2001:db8::1".parse().unwrap(),
                remote_port: 51234,
                state: "ESTABLISHED",
                pid: Some(u32::MAX),
                process_name: Some("browser-worker".to_string()),
                remote_host: Some("example.test".to_string()),
            },
            NetConnectionInfo {
                proto: "UDP",
                local_ip: "192.0.2.10".parse().unwrap(),
                local_port: 5353,
                local_host: None,
                remote_ip: Ipv4Addr::UNSPECIFIED.into(),
                remote_port: 0,
                state: "UNCONN",
                pid: None,
                process_name: None,
                remote_host: None,
            },
            NetConnectionInfo {
                proto: "CUSTOMPROTO",
                local_ip: "198.51.100.5".parse().unwrap(),
                local_port: 9000,
                local_host: Some("custom local".to_string()),
                remote_ip: "203.0.113.7".parse().unwrap(),
                remote_port: 9001,
                state: "CUSTOM STATE",
                pid: Some(99),
                process_name: Some("custom-service".to_string()),
                remote_host: None,
            },
        ]
    }

    fn assert_connections_equal(
        left: &Result<Vec<NetConnectionInfo>, &'static str>,
        right: &Result<Vec<NetConnectionInfo>, &'static str>,
    ) {
        match (left, right) {
            (Ok(left), Ok(right)) => {
                assert_eq!(left.len(), right.len());
                for (left, right) in left.iter().zip(right) {
                    assert_eq!(left.proto, right.proto);
                    assert_eq!(left.local_ip, right.local_ip);
                    assert_eq!(left.local_port, right.local_port);
                    assert_eq!(left.local_host, right.local_host);
                    assert_eq!(left.remote_ip, right.remote_ip);
                    assert_eq!(left.remote_port, right.remote_port);
                    assert_eq!(left.state, right.state);
                    assert_eq!(left.pid, right.pid);
                    assert_eq!(left.process_name, right.process_name);
                    assert_eq!(left.remote_host, right.remote_host);
                }
            }
            (Err(left), Err(right)) => assert_eq!(left, right),
            _ => panic!("connection result mismatch"),
        }
    }

    #[test]
    fn compact_materialize_and_recompact_preserve_every_process_and_connection_field() {
        let live = Snapshot {
            processes: vec![process_fixture(), ProcessInfo::default()],
            net_connections: Ok(connection_fixture()),
            ..Snapshot::default()
        };

        let mut historical = live.historical_copy();
        assert!(historical.history_is_compact());
        assert!(historical.processes.is_empty());
        assert!(historical.net_connections.as_ref().unwrap().is_empty());
        historical.materialize_history();
        assert!(!historical.history_is_compact());
        assert_processes_equal(&live.processes, &historical.processes);
        assert_connections_equal(&live.net_connections, &historical.net_connections);

        historical.compact_history();
        historical.compact_history();
        assert!(historical.processes.is_empty());
        let copied_compact = historical.historical_copy();
        let mut round_trip = copied_compact;
        round_trip.materialize_history();
        assert_processes_equal(&live.processes, &round_trip.processes);
        assert_connections_equal(&live.net_connections, &round_trip.net_connections);
    }

    #[test]
    fn compact_snapshot_preserves_network_collection_error() {
        let mut snapshot = Snapshot {
            net_connections: Err("permissions unavailable"),
            ..Snapshot::default()
        }
        .historical_copy();
        snapshot.materialize_history();
        assert_connections_equal(&Err("permissions unavailable"), &snapshot.net_connections);
    }
}
