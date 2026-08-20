//! Utility helpers for unit formatting, fuzzy matching, and clipboard support.

/// Formats a byte quantity dynamically into a human-readable string (B, KB, MB, GB, TB).
///
/// # Arguments
/// * `bytes` - The size in bytes as an `f64`.
///
/// # Returns
/// A formatted string such as `"4.2 MB"` or `"16.0 GB"`.
pub fn format_bytes_dyn(bytes: f64) -> String {
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

/// Formats a percentage value into a clean percentage string with 0 or 1 decimal place.
///
/// # Arguments
/// * `val` - Percentage value (0.0 to 100.0).
///
/// # Returns
/// Formatted string like `"0%"`, `"0.5%"`, or `"78%"`.
pub fn format_percent(val: f64) -> String {
    if val <= 0.0 {
        "0%".to_string()
    } else if val < 1.0 {
        let s = format!("{:.1}%", val);
        if s == "0.0%" {
            "0.1%".to_string()
        } else if s == "1.0%" {
            "1%".to_string()
        } else {
            s
        }
    } else {
        format!("{:.0}%", val.round())
    }
}

/// Formats a clock frequency in MHz into a human-readable frequency string (MHz or GHz).
///
/// # Arguments
/// * `mhz` - Clock frequency in megahertz.
///
/// # Returns
/// Formatted string like `"800 MHz"` or `"4.32 GHz"`.
pub fn format_freq(mhz: f64) -> String {
    if mhz >= 1000.0 {
        format!("{:.2} GHz", mhz / 1000.0)
    } else {
        format!("{:.0} MHz", mhz)
    }
}

/// Performs a case-insensitive subsequence fuzzy search matching pattern against text.
///
/// # Arguments
/// * `pattern` - Search query string.
/// * `text` - Target text to search within.
///
/// # Returns
/// `true` if `text` matches `pattern`, `false` otherwise.
pub fn fuzzy_match(pattern: &str, text: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    let pattern_lower = pattern.to_lowercase();
    let text_lower = text.to_lowercase();

    if text_lower.contains(&pattern_lower) {
        return true;
    }

    let mut pat_chars = pattern_lower.chars().peekable();
    for c in text_lower.chars() {
        if let Some(&p) = pat_chars.peek() {
            if c == p {
                pat_chars.next();
            }
        } else {
            return true;
        }
    }
    pat_chars.peek().is_none()
}

/// Parses a human-readable size string (e.g. "204.9MB", "1.63GB", "12.3kB", "0B") into raw bytes.
///
/// # Arguments
/// * `s` - Raw size string from Docker or system output.
///
/// # Returns
/// Parsed byte count as `u64`.
pub fn parse_size_to_bytes(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() || s == "0" || s == "0B" {
        return 0;
    }
    let num_part: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let unit_part: String = s
        .chars()
        .skip_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .trim()
        .to_uppercase();

    if let Ok(val) = num_part.parse::<f64>() {
        if unit_part.starts_with("TB") || unit_part.starts_with('T') {
            (val * 1024.0 * 1024.0 * 1024.0 * 1024.0) as u64
        } else if unit_part.starts_with("GB") || unit_part.starts_with('G') {
            (val * 1024.0 * 1024.0 * 1024.0) as u64
        } else if unit_part.starts_with("MB") || unit_part.starts_with('M') {
            (val * 1024.0 * 1024.0) as u64
        } else if unit_part.starts_with("KB") || unit_part.starts_with('K') {
            (val * 1024.0) as u64
        } else {
            val as u64
        }
    } else {
        0
    }
}

/// Formats system uptime given total seconds into a clean human-readable representation.
///
/// # Arguments
/// * `total_secs` - System uptime duration in seconds.
///
/// # Returns
/// Formatted string such as `"2d 4h 12m"`, `"3h 15m 42s"`, or `"12m 5s"`.
pub fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if days > 0 {
        format!("{}d {}h {}m", days, hours, minutes)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else {
        format!("{}m {}s", minutes, seconds)
    }
}

/// Copies arbitrary text to the system clipboard using Wayland `wl-copy`, X11 `xclip`/`xsel`,
/// or standard terminal OSC 52 escape sequences.
///
/// # Arguments
/// * `text` - String content to copy.
///
/// # Returns
/// `true` if copying was initiated successfully.
pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // 1. Try wl-copy (Wayland)
    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().is_ok() {
            return true;
        }
    }

    // 2. Try xclip (X11)
    if let Ok(mut child) = Command::new("xclip")
        .arg("-selection")
        .arg("clipboard")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().is_ok() {
            return true;
        }
    }

    // 3. Try xsel (X11)
    if let Ok(mut child) = Command::new("xsel")
        .arg("--clipboard")
        .arg("--input")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().is_ok() {
            return true;
        }
    }

    // 4. OSC 52 terminal clipboard escape sequence
    let encoded = base64_encode(text.as_bytes());
    print!("\x1b]52;c;{}\x07", encoded);
    let _ = std::io::stdout().flush();
    true
}

/// Encodes raw binary bytes into a Base64 string for OSC 52 transmission.
///
/// # Arguments
/// * `input` - Binary slice to encode.
///
/// # Returns
/// Base64 encoded string.
fn base64_encode(input: &[u8]) -> String {
    const CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i];
        let b1 = if i + 1 < input.len() { input[i + 1] } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] } else { 0 };

        out.push(CHARSET[(b0 >> 2) as usize] as char);
        out.push(CHARSET[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);

        if i + 1 < input.len() {
            out.push(CHARSET[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }

        if i + 2 < input.len() {
            out.push(CHARSET[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }

        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::prelude::*;
    use ratatui::widgets::{Block, Borders, Tabs};

    #[test]
    fn test_tabs_coords() {
        let area = Rect::new(0, 0, 100, 3);
        let mut buf = Buffer::empty(area);
        let titles = vec![
            Line::from("General (1)"),
            Line::from("Processes (2)"),
            Line::from("CPU & RAM (3)"),
            Line::from("GPU (4)"),
            Line::from("Network (5)"),
            Line::from("Disk (6)"),
        ];
        let tabs = Tabs::new(titles)
            .select(0)
            .block(Block::default().borders(Borders::ALL))
            .divider("|")
            .padding(" ", " ");
        tabs.render(area, &mut buf);
        let row: String = (0..area.width).map(|x| buf[(x, 1)].symbol().chars().next().unwrap_or(' ')).collect();
        println!("ROW: '{}'", row);

        let row_chars: Vec<char> = (0..area.width)
            .map(|x| buf[(x, 1)].symbol().chars().next().unwrap_or(' '))
            .collect();

        let titles_raw = [
            "General (1)",
            "Processes (2)",
            "CPU & RAM (3)",
            "GPU (4)",
            "Network (5)",
            "Disk (6)",
        ];
        let mut tab_x = area.x + 1;
        for (idx, title) in titles_raw.iter().enumerate() {
            let tab_w = 1 + title.chars().count() as u16 + 1;
            let slice: String = row_chars[tab_x as usize..(tab_x + tab_w) as usize].iter().collect();
            println!("Tab {}: idx={} range=[{}, {}) text='{}'", idx, title, tab_x, tab_x + tab_w, slice);
            assert_eq!(slice, format!(" {} ", title));
            if idx < titles_raw.len() - 1 {
                let div: String = row_chars[(tab_x + tab_w) as usize..(tab_x + tab_w + 1) as usize].iter().collect();
                assert_eq!(div, "|");
            }
            tab_x += tab_w + 1;
        }
    }
}
