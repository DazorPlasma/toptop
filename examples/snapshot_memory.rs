//! Measures retained heap allocations for 300 real telemetry snapshots.
//!
//! Run with `cargo run --release --example snapshot_memory -- --compact-only`.
//! The example reads this host's `/proc` data, keeps 300 archived snapshots
//! alive, and checks one selected frame after expanding and re-compacting it.

#![allow(dead_code)]

use std::{
    alloc::{GlobalAlloc, Layout},
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

#[path = "../src/process.rs"]
mod process;
#[path = "../src/process_network.rs"]
mod process_network;
#[path = "../src/snapshot.rs"]
mod snapshot;
#[path = "../src/system.rs"]
mod system;
#[path = "../src/utils.rs"]
mod utils;

const SNAPSHOT_COUNT: usize = 300;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static TRACKED_BYTES: AtomicUsize = AtomicUsize::new(0);

struct TrackingAllocator;

static REAL_ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

// This wrapper counts requested live bytes while delegating allocation to the
// same allocator used by the application binary.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { REAL_ALLOCATOR.alloc(layout) };
        if !ptr.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            TRACKED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { REAL_ALLOCATOR.alloc_zeroed(layout) };
        if !ptr.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            TRACKED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            TRACKED_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        unsafe { REAL_ALLOCATOR.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { REAL_ALLOCATOR.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            TRACKED_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
            TRACKED_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn sample(tick: usize) -> snapshot::MetricSample {
    let value = tick as f64;
    snapshot::MetricSample {
        cpu: value % 100.0,
        mem: (value + 1.0) % 100.0,
        swap: (value + 2.0) % 100.0,
        gpu: (tick % 2 == 0).then_some(value % 100.0),
        gpu_vram: (value + 3.0) % 100.0,
        net_rx: value * 11.0,
        net_tx: value * 13.0,
        disk_read: value * 17.0,
        disk_write: value * 19.0,
    }
}

fn tracked<T>(f: impl FnOnce() -> T) -> (T, usize) {
    TRACKED_BYTES.store(0, Ordering::SeqCst);
    TRACK_ALLOCATIONS.store(true, Ordering::SeqCst);
    let result = f();
    TRACK_ALLOCATIONS.store(false, Ordering::SeqCst);
    let bytes = TRACKED_BYTES.load(Ordering::SeqCst);
    (result, bytes)
}

fn rss_kib() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

fn prepare_snapshot() -> snapshot::Snapshot {
    let mut previous_processes = HashMap::new();
    let processes = process::read_processes(&mut previous_processes, &HashMap::new(), 2.0);

    let network_connections = {
        let dns = system::DnsResolver::new();
        system::read_network_connections(&dns)
    };
    // Let any async reverse lookup requests finish before the allocator window.
    thread::sleep(Duration::from_millis(500));

    let mut io_buf = String::new();
    let mem = system::read_memory(&mut io_buf);
    let (cpu_cur_mhz, cpu_min_mhz, cpu_max_mhz) = system::read_cpu_freq_info();
    let mut previous_net = HashMap::new();
    let mut previous_disk = HashMap::new();

    let mut live = snapshot::Snapshot::default();
    live.sys_info = system::read_system_general_info();
    live.battery = system::read_battery();
    live.global_usage = 18.25;
    live.core_usages = vec![10.5, 20.25, 30.75, 40.0];
    live.cpu_cur_mhz = cpu_cur_mhz;
    live.cpu_min_mhz = cpu_min_mhz;
    live.cpu_max_mhz = cpu_max_mhz;
    live.cpu_temp = system::read_cpu_temp();
    live.ram_temp = system::read_ram_temp();
    live.mem = mem;
    live.ram_info = "benchmark host memory".to_string();
    live.gpu_metrics = system::read_gpu_metrics();
    live.net_ifaces = system::read_network_interfaces(&mut previous_net, 2.0);
    live.net_connections = network_connections;
    live.disk_io = system::read_disk_io(&mut previous_disk, 2.0);
    live.disk_mounts = system::read_disk_mounts();
    live.processes = processes;
    live.process_net_source = process_network::NetworkSource::Unavailable;

    live.history.seed(
        live.global_usage,
        live.mem_used_pct(),
        live.swap_used_pct(),
        live.gpu_metrics
            .utilization_available
            .then_some(live.gpu_metrics.utilization_pct),
        live.vram_used_pct(),
    );
    live
}

fn check_display_fidelity(
    source: &snapshot::Snapshot,
    copied: &snapshot::Snapshot,
    check_chart_history: bool,
) {
    assert!(!copied.history_is_compact());
    assert_eq!(copied.sys_info.hostname, source.sys_info.hostname);
    assert_eq!(copied.sys_info.os_name, source.sys_info.os_name);
    assert_eq!(copied.sys_info.kernel, source.sys_info.kernel);
    assert_eq!(copied.sys_info.uptime_secs, source.sys_info.uptime_secs);
    assert_eq!(copied.sys_info.desktop, source.sys_info.desktop);
    assert_eq!(copied.sys_info.shell, source.sys_info.shell);
    assert_eq!(copied.sys_info.local_ip, source.sys_info.local_ip);
    assert_eq!(copied.sys_info.net_interface, source.sys_info.net_interface);
    assert_eq!(
        copied.sys_info.net_speed_mbps,
        source.sys_info.net_speed_mbps
    );
    assert_eq!(copied.sys_info.net_duplex, source.sys_info.net_duplex);
    assert_eq!(copied.sys_info.locale, source.sys_info.locale);
    assert_eq!(
        copied.sys_info.displays.len(),
        source.sys_info.displays.len()
    );
    match (&source.battery, &copied.battery) {
        (Some(original), Some(saved)) => {
            assert_eq!(saved.name, original.name);
            assert_eq!(saved.status, original.status);
            assert_eq!(saved.capacity_pct, original.capacity_pct);
            assert_eq!(saved.energy_now_wh, original.energy_now_wh);
            assert_eq!(saved.energy_full_wh, original.energy_full_wh);
            assert_eq!(saved.power_w, original.power_w);
            assert_eq!(saved.health, original.health);
            assert_eq!(saved.power_plan, original.power_plan);
        }
        (None, None) => {}
        _ => panic!("historical copy changed battery availability"),
    }
    assert_eq!(copied.global_usage, source.global_usage);
    assert_eq!(copied.core_usages, source.core_usages);
    assert_eq!(copied.cpu_cur_mhz, source.cpu_cur_mhz);
    assert_eq!(copied.cpu_min_mhz, source.cpu_min_mhz);
    assert_eq!(copied.cpu_max_mhz, source.cpu_max_mhz);
    assert_eq!(copied.cpu_temp, source.cpu_temp);
    assert_eq!(copied.ram_temp, source.ram_temp);
    assert_eq!(copied.mem.used_mem_mb, source.mem.used_mem_mb);
    assert_eq!(copied.mem.total_mem_mb, source.mem.total_mem_mb);
    assert_eq!(copied.mem.used_swap_mb, source.mem.used_swap_mb);
    assert_eq!(copied.mem.total_swap_mb, source.mem.total_swap_mb);
    assert_eq!(copied.ram_info, source.ram_info);
    assert_eq!(copied.gpu_metrics.name, source.gpu_metrics.name);
    assert_eq!(
        copied.gpu_metrics.utilization_pct,
        source.gpu_metrics.utilization_pct
    );
    assert_eq!(
        copied.gpu_metrics.vram_used_mb,
        source.gpu_metrics.vram_used_mb
    );
    assert_eq!(copied.net_ifaces.len(), source.net_ifaces.len());
    for (original, saved) in source.net_ifaces.iter().zip(&copied.net_ifaces) {
        assert_eq!(saved.name, original.name);
        assert_eq!(saved.rx_bytes, original.rx_bytes);
        assert_eq!(saved.tx_bytes, original.tx_bytes);
        assert_eq!(saved.rx_speed, original.rx_speed);
        assert_eq!(saved.tx_speed, original.tx_speed);
        assert_eq!(saved.mac, original.mac);
        assert_eq!(saved.operstate, original.operstate);
        assert_eq!(saved.speed_mbps, original.speed_mbps);
        assert_eq!(saved.duplex, original.duplex);
    }
    match (&source.net_connections, &copied.net_connections) {
        (Ok(original), Ok(saved)) => {
            assert_eq!(saved.len(), original.len());
            for (original, saved) in original.iter().zip(saved) {
                assert_eq!(saved.proto, original.proto);
                assert_eq!(saved.local_ip, original.local_ip);
                assert_eq!(saved.local_port, original.local_port);
                assert_eq!(saved.local_host, original.local_host);
                assert_eq!(saved.remote_ip, original.remote_ip);
                assert_eq!(saved.remote_port, original.remote_port);
                assert_eq!(saved.state, original.state);
                assert_eq!(saved.pid, original.pid);
                assert_eq!(saved.process_name, original.process_name);
                assert_eq!(saved.remote_host, original.remote_host);
            }
        }
        (Err(original), Err(saved)) => assert_eq!(saved, original),
        _ => panic!("historical copy changed connection availability"),
    }
    assert_eq!(
        copied.disk_io.total_read_bytes,
        source.disk_io.total_read_bytes
    );
    assert_eq!(
        copied.disk_io.total_write_bytes,
        source.disk_io.total_write_bytes
    );
    assert_eq!(copied.disk_mounts.len(), source.disk_mounts.len());
    assert_eq!(
        std::mem::discriminant(&copied.process_net_source),
        std::mem::discriminant(&source.process_net_source)
    );
    assert_eq!(copied.processes.len(), source.processes.len());

    for (original, saved) in source.processes.iter().zip(&copied.processes) {
        assert_eq!(saved.pid, original.pid);
        assert_eq!(saved.ppid, original.ppid);
        assert_eq!(saved.name.as_ref(), original.name.as_ref());
        assert_eq!(saved.comm.as_ref(), original.comm.as_ref());
        assert_eq!(saved.state, original.state);
        assert_eq!(saved.rss_kb, original.rss_kb);
        assert_eq!(saved.starttime_ticks, original.starttime_ticks);
        assert_eq!(saved.utime, original.utime);
        assert_eq!(saved.stime, original.stime);
        assert_eq!(saved.read_bytes, original.read_bytes);
        assert_eq!(saved.write_bytes, original.write_bytes);
        assert_eq!(saved.threads, original.threads);
        assert_eq!(saved.uid, original.uid);
        assert_eq!(saved.user.as_ref(), original.user.as_ref());
        assert_eq!(saved.cpu_percent, original.cpu_percent);
        assert_eq!(saved.read_speed, original.read_speed);
        assert_eq!(saved.write_speed, original.write_speed);
        assert_eq!(saved.net_rx_speed, original.net_rx_speed);
        assert_eq!(saved.net_tx_speed, original.net_tx_speed);
        assert_eq!(saved.net_partial, original.net_partial);
        assert_eq!(saved.gpu_percent, original.gpu_percent);
        assert_eq!(saved.gpu_mem_kb, original.gpu_mem_kb);
        assert_eq!(saved.pss_kb, original.pss_kb);
        assert_eq!(saved.grouped_pids, original.grouped_pids);
        assert_eq!(saved.is_group_header, original.is_group_header);
        assert_eq!(saved.is_group_child, original.is_group_child);
        assert_eq!(saved.group_name, original.group_name);
        match (&original.gpu_sample, &saved.gpu_sample) {
            (Some(before), Some(after)) => {
                assert_eq!(after.engine_ns, before.engine_ns);
                assert_eq!(after.engine_capacity, before.engine_capacity);
                assert_eq!(after.engine_percent, before.engine_percent);
                assert_eq!(after.counter_regressed, before.counter_regressed);
            }
            (None, None) => {}
            _ => panic!("historical copy changed GPU sample availability"),
        }
    }

    if check_chart_history {
        for metric in [
            snapshot::MetricKind::Cpu,
            snapshot::MetricKind::Mem,
            snapshot::MetricKind::Swap,
            snapshot::MetricKind::Gpu,
            snapshot::MetricKind::GpuVram,
            snapshot::MetricKind::NetRx,
            snapshot::MetricKind::NetTx,
            snapshot::MetricKind::DiskRead,
            snapshot::MetricKind::DiskWrite,
        ] {
            assert_eq!(copied.history.series(metric), source.history.series(metric));
        }
    }
}

fn retained_archive_bytes(source: &snapshot::Snapshot) -> usize {
    let (copies, bytes) = tracked(|| {
        let mut copies = Vec::with_capacity(SNAPSHOT_COUNT);
        for _ in 0..SNAPSHOT_COUNT {
            copies.push(source.historical_copy());
        }
        copies
    });
    assert_eq!(copies.len(), SNAPSHOT_COUNT);
    drop(copies);
    bytes
}

fn main() {
    let compact_only = std::env::args().any(|arg| arg == "--compact-only");
    let mut live = prepare_snapshot();
    let mut fidelity_copy = live.historical_copy();
    assert!(fidelity_copy.history_is_compact());
    fidelity_copy.materialize_history();
    check_display_fidelity(&live, &fidelity_copy, true);
    fidelity_copy.compact_history();
    assert!(fidelity_copy.history_is_compact());
    drop(fidelity_copy);

    // Grow the live history to its steady-state capacity before measuring so
    // pre-window VecDeque reallocations do not contaminate the allocation count.
    for tick in 1..snapshot::HISTORY_LEN {
        live.history.push(sample(tick));
    }

    let process_count = live.processes.len();
    let connection_count = live.net_connections.as_ref().map(Vec::len).unwrap_or(0);
    let gpu_sample_count = live
        .processes
        .iter()
        .filter(|process| process.gpu_sample.is_some())
        .count();
    let rss_before = rss_kib();
    let (mut snapshots, compact_bytes) = tracked(|| {
        let mut snapshots = Vec::new();
        for tick in 0..SNAPSHOT_COUNT {
            if tick > 0 {
                live.history.push(sample(snapshot::HISTORY_LEN + tick));
            }
            snapshots.push(live.historical_copy());
        }
        assert_eq!(snapshots.len(), SNAPSHOT_COUNT);
        snapshots
    });
    assert_eq!(compact_bytes, TRACKED_BYTES.load(Ordering::SeqCst));
    let rss_archived = rss_kib();
    assert_eq!(snapshots.len(), SNAPSHOT_COUNT);

    // Re-enable accounting only for the one selected frame expansion. The
    // counter already reflects the retained compact ring, so deallocation on
    // re-compaction must return to precisely the same value.
    let compact_baseline = TRACKED_BYTES.load(Ordering::SeqCst);
    TRACK_ALLOCATIONS.store(true, Ordering::SeqCst);
    let selected_index = SNAPSHOT_COUNT / 2;
    snapshots[selected_index].materialize_history();
    let one_frame_materialized_bytes = TRACKED_BYTES.load(Ordering::SeqCst);
    check_display_fidelity(&live, &snapshots[selected_index], false);
    snapshots[selected_index].compact_history();
    assert!(snapshots[selected_index].history_is_compact());
    let recompressed_bytes = TRACKED_BYTES.load(Ordering::SeqCst);
    TRACK_ALLOCATIONS.store(false, Ordering::SeqCst);
    assert_eq!(compact_baseline, recompressed_bytes);
    let rss_after_one_frame = rss_kib();

    // Exercise repeated time-travel navigation without expanding the ring at
    // once. Restore the counter to the retained ring total because the
    // component breakdown below uses independent tracking windows.
    TRACKED_BYTES.store(compact_bytes, Ordering::SeqCst);
    TRACK_ALLOCATIONS.store(true, Ordering::SeqCst);
    let mut navigation_peak_bytes = compact_bytes;
    for _ in 0..3 {
        for index in [0, SNAPSHOT_COUNT / 2, SNAPSHOT_COUNT - 1] {
            snapshots[index].materialize_history();
            navigation_peak_bytes = navigation_peak_bytes.max(TRACKED_BYTES.load(Ordering::SeqCst));
            check_display_fidelity(&live, &snapshots[index], false);
            snapshots[index].compact_history();
            assert!(snapshots[index].history_is_compact());
            assert_eq!(TRACKED_BYTES.load(Ordering::SeqCst), compact_bytes);
        }
    }
    TRACK_ALLOCATIONS.store(false, Ordering::SeqCst);
    let rss_after_navigation = rss_kib();

    // Measure the incremental row archive allocations by removing one row
    // class at a time. Each run has identical Snapshot and ring overhead, so
    // subtracting the empty-row run isolates that class's retained archive.
    let mut empty_rows = live.clone();
    empty_rows.processes.clear();
    empty_rows.net_connections = Ok(Vec::new());
    let base_bytes = retained_archive_bytes(&empty_rows);

    let mut process_only = live.clone();
    process_only.net_connections = Ok(Vec::new());
    let process_archive_bytes = retained_archive_bytes(&process_only).saturating_sub(base_bytes);

    let mut connections_only = live.clone();
    connections_only.processes.clear();
    let connection_archive_bytes =
        retained_archive_bytes(&connections_only).saturating_sub(base_bytes);
    let component_difference = compact_bytes as i128
        - (base_bytes + process_archive_bytes + connection_archive_bytes) as i128;

    println!("snapshot count: {SNAPSHOT_COUNT} (startup + 299 ticks)");
    println!(
        "benchmark mode: {}",
        if compact_only {
            "compact-only"
        } else {
            "compact + one-frame fidelity"
        }
    );
    println!("host process records: {process_count}");
    println!("host socket records: {connection_count}");
    println!("processes with GPU engine samples: {gpu_sample_count}");
    println!(
        "compact archived snapshots: {compact_bytes} bytes ({:.2} MiB)",
        compact_bytes as f64 / 1_048_576.0
    );
    println!(
        "  common snapshot + metric history: {base_bytes} bytes ({:.2} MiB)",
        base_bytes as f64 / 1_048_576.0
    );
    println!(
        "  process archive increment: {process_archive_bytes} bytes ({:.2} MiB)",
        process_archive_bytes as f64 / 1_048_576.0
    );
    println!(
        "  connection archive increment: {connection_archive_bytes} bytes ({:.2} MiB)",
        connection_archive_bytes as f64 / 1_048_576.0
    );
    println!(
        "  process + connection archive: {} bytes ({:.2} MiB)",
        process_archive_bytes + connection_archive_bytes,
        (process_archive_bytes + connection_archive_bytes) as f64 / 1_048_576.0
    );
    println!(
        "  per-tick history / combined-copy remainder: {component_difference} bytes ({:.2} MiB)",
        component_difference as f64 / 1_048_576.0
    );
    println!(
        "single selected frame expanded allocation peak: {} bytes (+{} bytes)",
        one_frame_materialized_bytes,
        one_frame_materialized_bytes.saturating_sub(compact_baseline)
    );
    println!(
        "compact-ring VmRSS increase: {} KiB",
        rss_archived.saturating_sub(rss_before)
    );
    println!(
        "VmRSS after one-frame expand/recompact: {} KiB above initial",
        rss_after_one_frame.saturating_sub(rss_before)
    );
    println!(
        "navigation stress: 9 expand/check/recompact cycles, peak {} bytes ({:+} bytes over compact ring), RSS +{} KiB",
        navigation_peak_bytes,
        navigation_peak_bytes as isize - compact_bytes as isize,
        rss_after_navigation.saturating_sub(rss_before)
    );

    if compact_only {
        assert!(snapshots.iter().all(snapshot::Snapshot::history_is_compact));
    }
}
