use std::fs;
use std::thread;
use std::time::Duration;

fn main() {
    let pid = std::process::id();
    let read_ticks = || {
        let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).unwrap();
        let rparen = stat.rfind(')').unwrap();
        let rest = &stat[rparen + 1..];
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let utime: u64 = parts[11].parse().unwrap();
        let stime: u64 = parts[12].parse().unwrap();
        utime + stime
    };
    
    let t1 = read_ticks();
    
    // Busy loop
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
    }
    
    let dt = start.elapsed().as_secs_f64();
    let t2 = read_ticks();
    
    println!("delta_ticks: {}, dt: {}", t2 - t1, dt);
    println!("cpu_percent: {}", (t2 - t1) as f64 / dt);
}
