//! Theme and color gradient calculation module.
//!
//! Provides color utilities for resource visualization, including multi-stop
//! linear gradients (green -> yellow -> orange -> red -> violet) and I/O scaling.

use ratatui::style::Color;

/// Computes a smooth color along a multi-stop green-yellow-orange-red gradient
/// based on a percentage value from 0.0 to 100.0.
///
/// # Arguments
/// * `pct` - Percentage value between 0.0 and 100.0.
///
/// # Returns
/// An RGB `Color` representing the intensity level.
pub fn gradient_color(pct: f64) -> Color {
    let pct = pct.clamp(0.0, 100.0);
    let (r, g, b) = if pct <= 5.0 {
        let t = pct / 5.0;
        let g = (130.0 + 25.0 * t).round() as u8;
        (0, g, 0)
    } else if pct <= 20.0 {
        let t = (pct - 5.0) / 15.0;
        let g = (155.0 + 100.0 * t).round() as u8;
        (0, g, 0)
    } else if pct <= 55.0 {
        let t = (pct - 20.0) / 35.0;
        let r = (255.0 * t).round() as u8;
        (r, 255, 0)
    } else if pct <= 85.0 {
        let t = (pct - 55.0) / 30.0;
        let g = (255.0 - 127.0 * t).round() as u8;
        (255, g, 0)
    } else {
        let t = (pct - 85.0) / 15.0;
        let g = (128.0 * (1.0 - t)).round() as u8;
        (255, g, 0)
    };
    Color::Rgb(r, g, b)
}

/// Calculates the color for process CPU usage, scaling up into vivid violet/purple
/// for multi-threaded processes exceeding single-core and multi-core thresholds.
///
/// # Arguments
/// * `cpu_pct` - Aggregate CPU usage percent for the process.
/// * `num_cores` - Total number of available CPU logical cores.
///
/// # Returns
/// An RGB `Color` representing process CPU activity.
pub fn process_cpu_color(cpu_pct: f64, num_cores: usize) -> Color {
    let num_cores = num_cores.max(1) as f64;
    let total_capacity = num_cores * 100.0;
    let violet_threshold = 0.80 * total_capacity;

    if cpu_pct > violet_threshold {
        let span = (total_capacity - violet_threshold).max(1.0);
        let t = ((cpu_pct - violet_threshold) / span).clamp(0.0, 1.0);
        let r = (255.0 - 75.0 * t).round() as u8;
        let g = 0;
        let b = (255.0 * t).round() as u8;
        Color::Rgb(r, g, b)
    } else if cpu_pct > 100.0 {
        Color::Rgb(255, 0, 0)
    } else {
        gradient_color(cpu_pct)
    }
}

/// Scales disk and network I/O throughput into a non-linear percentage (0.0 to 100.0)
/// suitable for color gradient mapping.
///
/// # Arguments
/// * `speed` - Throughput speed in bytes per second.
///
/// # Returns
/// A float between 0.0 and 100.0 indicating I/O saturation.
pub fn io_gradient_pct(speed: f64) -> f64 {
    if speed <= 1024.0 {
        0.0
    } else if speed <= 10.0 * 1024.0 * 1024.0 {
        (speed / (10.0 * 1024.0 * 1024.0)) * 40.0
    } else if speed <= 50.0 * 1024.0 * 1024.0 {
        40.0 + ((speed - 10.0 * 1024.0 * 1024.0) / (40.0 * 1024.0 * 1024.0)) * 40.0
    } else {
        (80.0 + ((speed - 50.0 * 1024.0 * 1024.0) / (50.0 * 1024.0 * 1024.0)) * 20.0).min(100.0)
    }
}

/// Darkens an RGB color by multiplying its components with a scaling factor.
///
/// # Arguments
/// * `c` - Original color.
/// * `factor` - Brightness multiplier (e.g. 0.3 for 30% brightness).
///
/// # Returns
/// The darkened RGB `Color`.
pub fn darken_color(c: Color, factor: f64) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            ((r as f64) * factor).round() as u8,
            ((g as f64) * factor).round() as u8,
            ((b as f64) * factor).round() as u8,
        ),
        _ => c,
    }
}
