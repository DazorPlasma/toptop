//! Per-process network sampling with an unprivileged TCP fallback.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    net::SocketAddr,
    process::{Child, Command, Stdio},
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

use crate::process::ProcessInfo;

/// Scope of the network numbers shown in a process snapshot.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum NetworkSource {
    /// No usable per-process measurement was available.
    #[default]
    Unavailable,
    /// TCP payload bytes from socket diagnostics, for attributable sockets only.
    TcpPayload,
    /// Observed TCP and UDP packets from a privileged NetHogs capture.
    PacketCapture,
}

/// One TCP socket's counters and process identity.
struct TcpCounters {
    pid: u32,
    starttime_ticks: u64,
    endpoints: EndpointIdentity,
    sent: u64,
    received: u64,
    sample_generation: u64,
}

/// Exact endpoint identity, encoded compactly when `ss` reports numeric IP addresses and ports.
#[derive(PartialEq, Eq)]
enum EndpointIdentity {
    IpAndPort([u8; 46]),
    /// Preserve unusual `ss` endpoint spellings exactly when they do not parse as `SocketAddr`.
    Raw(String),
}

/// Rates and identity from one NetHogs process row.
struct CaptureRate {
    rx: f64,
    tx: f64,
    starttime_ticks: u64,
}

/// A complete NetHogs trace refresh, received from its reader thread.
type CaptureFrame = (Instant, HashMap<u32, CaptureRate>);

/// Long-running privileged capture process and its latest parsed refreshes.
struct Capture {
    child: Child,
    frames: Receiver<CaptureFrame>,
    latest: Option<CaptureFrame>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Samples measured per-process network traffic without blocking on packet capture.
pub struct ProcessNetworkCollector {
    previous_tcp: HashMap<u64, TcpCounters>,
    tcp_generation: u64,
    previous_tcp_at: Option<Instant>,
    capture: Option<Capture>,
}

impl ProcessNetworkCollector {
    /// Starts packet capture when already privileged, with socket diagnostics as fallback.
    pub fn new() -> Self {
        let capture = if rustix::process::geteuid().as_raw() == 0 {
            start_capture()
        } else {
            None
        };
        Self {
            previous_tcp: HashMap::new(),
            tcp_generation: 0,
            previous_tcp_at: None,
            capture,
        }
    }

    /// Updates network rates on the just-read process list and returns their source.
    pub fn sample(&mut self, processes: &mut [ProcessInfo]) -> NetworkSource {
        if let Some(capture) = self.capture.as_mut() {
            while let Ok(frame) = capture.frames.try_recv() {
                capture.latest = Some(frame);
            }
            let running = matches!(capture.child.try_wait(), Ok(None));
            if running {
                if let Some((captured_at, rates)) = capture.latest.as_ref()
                    && captured_at.elapsed() < Duration::from_secs(5)
                {
                    for process in processes {
                        if let Some(rate) = rates.get(&process.pid)
                            && rate.starttime_ticks != 0
                            && rate.starttime_ticks == process.starttime_ticks
                        {
                            process.net_rx_speed = Some(rate.rx);
                            process.net_tx_speed = Some(rate.tx);
                        }
                    }
                    self.previous_tcp.clear();
                    self.previous_tcp_at = None;
                    return NetworkSource::PacketCapture;
                }
            } else {
                self.capture = None;
            }
        }

        self.sample_tcp(processes)
    }

    /// Derives TCP payload rates from consecutive `ss` socket counters.
    fn sample_tcp(&mut self, processes: &mut [ProcessInfo]) -> NetworkSource {
        let sampled_at = Instant::now();
        let output = match Command::new("ss").args(["-tienpHOn"]).output() {
            Ok(output) if output.status.success() => output,
            _ => {
                self.previous_tcp.clear();
                self.previous_tcp_at = None;
                return NetworkSource::Unavailable;
            }
        };
        let identities: HashMap<u32, u64> = processes
            .iter()
            .map(|process| (process.pid, process.starttime_ticks))
            .collect();
        let dt = self
            .previous_tcp_at
            .map(|previous| sampled_at.duration_since(previous).as_secs_f64())
            .unwrap_or(0.0);
        self.tcp_generation = self.tcp_generation.wrapping_add(1);
        if self.tcp_generation == 0 {
            self.previous_tcp.clear();
            self.tcp_generation = 1;
        }
        let generation = self.tcp_generation;
        let mut totals: HashMap<u32, (u64, u64)> = HashMap::new();

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((inode, pid, endpoints, sent, received)) = parse_ss_line(line) else {
                continue;
            };
            let Some(&starttime_ticks) = identities.get(&pid) else {
                continue;
            };
            let counters = TcpCounters {
                pid,
                starttime_ticks,
                endpoints,
                sent,
                received,
                sample_generation: generation,
            };
            if let Some(previous) = self.previous_tcp.get(&inode)
                && previous.sample_generation != generation
                && previous.pid == pid
                && previous.starttime_ticks == starttime_ticks
                && previous.endpoints == counters.endpoints
                && sent >= previous.sent
                && received >= previous.received
                && dt > 0.0
            {
                let total = totals.entry(pid).or_insert((0u64, 0u64));
                total.0 = total.0.saturating_add(received - previous.received);
                total.1 = total.1.saturating_add(sent - previous.sent);
            }
            self.previous_tcp.insert(inode, counters);
        }

        for process in processes {
            if let Some(&(rx, tx)) = totals.get(&process.pid) {
                process.net_rx_speed = Some(rx as f64 / dt);
                process.net_tx_speed = Some(tx as f64 / dt);
            }
        }
        self.previous_tcp
            .retain(|_, counters| counters.sample_generation == generation);
        if self.previous_tcp.capacity() > self.previous_tcp.len().saturating_mul(2) {
            self.previous_tcp.shrink_to_fit();
        }
        self.previous_tcp_at = Some(sampled_at);
        NetworkSource::TcpPayload
    }
}

/// Parses a single `ss -tienpHOn` row, rejecting sockets with ambiguous owners.
fn parse_ss_line(line: &str) -> Option<(u64, u32, EndpointIdentity, u64, u64)> {
    let mut columns = line.split_whitespace();
    columns.next()?; // TCP state.
    columns.next()?; // Receive queue.
    columns.next()?; // Send queue.
    let local = columns.next()?;
    let remote = columns.next()?;
    let field = |prefix: &str| -> Option<u64> {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(prefix)?.parse().ok())
    };
    let inode = field("ino:")?;
    let sent = field("bytes_sent:")?;
    let received = field("bytes_received:")?;
    let mut owners = line.match_indices("pid=").filter_map(|(offset, _)| {
        let value = &line[offset + 4..];
        let digit_end = value
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(value.len());
        value[..digit_end].parse::<u32>().ok()
    });
    let pid = owners.next()?;
    if owners.any(|other| other != pid) {
        return None;
    }
    Some((inode, pid, endpoint_identity(local, remote), sent, received))
}

/// Encodes numeric socket endpoints as exact IP/port tuples without heap allocation.
fn endpoint_identity(local: &str, remote: &str) -> EndpointIdentity {
    let (Some(local), Some(remote)) = (encode_endpoint(local), encode_endpoint(remote)) else {
        return EndpointIdentity::Raw(format!("{local}\0{remote}"));
    };
    let mut encoded = [0; 46];
    encoded[..23].copy_from_slice(&local);
    encoded[23..].copy_from_slice(&remote);
    EndpointIdentity::IpAndPort(encoded)
}

/// Encodes one parsed IP address and port in a fixed 23-byte representation.
fn encode_endpoint(endpoint: &str) -> Option<[u8; 23]> {
    let endpoint = endpoint.parse::<SocketAddr>().ok()?;
    let mut encoded = [0; 23];
    match endpoint {
        SocketAddr::V4(endpoint) => {
            encoded[0] = 4;
            encoded[1..5].copy_from_slice(&endpoint.ip().octets());
            encoded[17..19].copy_from_slice(&endpoint.port().to_be_bytes());
        }
        SocketAddr::V6(endpoint) => {
            encoded[0] = 6;
            encoded[1..17].copy_from_slice(&endpoint.ip().octets());
            encoded[17..19].copy_from_slice(&endpoint.port().to_be_bytes());
            encoded[19..23].copy_from_slice(&endpoint.scope_id().to_be_bytes());
        }
    }
    Some(encoded)
}

/// Starts NetHogs without invoking sudo or changing the current user's privileges.
fn start_capture() -> Option<Capture> {
    let mut child = Command::new("nethogs")
        .args(["-t", "-d", "1", "-C", "-a"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, frames) = channel();
    std::thread::spawn(move || {
        let mut rates = HashMap::new();
        let mut started = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.trim() == "Refreshing:" {
                if started {
                    let complete = rates
                        .into_iter()
                        .map(|(pid, (rx, tx))| {
                            (
                                pid,
                                CaptureRate {
                                    rx,
                                    tx,
                                    starttime_ticks: read_starttime_ticks(pid).unwrap_or(0),
                                },
                            )
                        })
                        .collect();
                    if sender.send((Instant::now(), complete)).is_err() {
                        break;
                    }
                }
                rates = HashMap::new();
                started = true;
            } else if started && let Some((pid, rx, tx)) = parse_capture_line(&line) {
                let entry = rates.entry(pid).or_insert((0.0f64, 0.0f64));
                entry.0 += rx;
                entry.1 += tx;
            }
        }
    });
    Some(Capture {
        child,
        frames,
        latest: None,
    })
}

/// Parses NetHogs trace rows (`name/pid/uid sent_kib_s recv_kib_s`).
fn parse_capture_line(line: &str) -> Option<(u32, f64, f64)> {
    let (prefix, recv_value) = line.rsplit_once('\t')?;
    let (name_pid_uid, sent_value) = prefix.rsplit_once('\t')?;
    let sent_kib_s = sent_value.parse::<f64>().ok()?;
    let recv_kib_s = recv_value.parse::<f64>().ok()?;
    if !sent_kib_s.is_finite() || !recv_kib_s.is_finite() || sent_kib_s < 0.0 || recv_kib_s < 0.0 {
        return None;
    }
    let mut identity = name_pid_uid.rsplitn(3, '/');
    identity.next()?.parse::<u32>().ok()?; // UID
    let pid = identity.next()?.parse::<u32>().ok()?;
    if pid == 0 {
        return None;
    }
    Some((pid, recv_kib_s * 1024.0, sent_kib_s * 1024.0))
}

/// Reads the kernel start time used to reject reused PIDs in capture frames.
fn read_starttime_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace().nth(19)?.parse().ok()
}
