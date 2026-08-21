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

/// Returns true if the kernel version string is considered an official LTS release or the latest series.
pub fn is_lts_or_latest_kernel(kernel_str: &str) -> bool {
    let lower = kernel_str.to_lowercase();
    if lower.contains("lts") {
        return true;
    }

    let parts: Vec<&str> = kernel_str.split(['.', '-', '_']).collect();

    if parts.len() >= 2 {
        let major = parts[0].parse::<u32>().ok();
        let minor = parts[1].parse::<u32>().ok();

        if let (Some(maj), Some(min)) = (major, minor) {
            // Known official Longterm (LTS) kernel releases:
            // 6.12, 6.6, 6.1, 5.15, 5.10, 5.4, 4.19, 4.14, 4.9, 4.4, 3.16
            let is_lts = matches!(
                (maj, min),
                (6, 12)
                    | (6, 6)
                    | (6, 1)
                    | (5, 15)
                    | (5, 10)
                    | (5, 4)
                    | (4, 19)
                    | (4, 14)
                    | (4, 9)
                    | (4, 4)
                    | (3, 16)
            );

            if is_lts {
                return true;
            }

            // Consider kernels at or above 6.13 (or >= 7.0) as the modern / latest series
            if (maj == 6 && min >= 13) || maj >= 7 {
                return true;
            }
        }
    }

    false
}

/// Returns true if the formatted RAM string represents capacity less than 8GB.
pub fn is_ram_under_8gb(ram_info: &str) -> bool {
    let lower = ram_info.to_lowercase();
    if let Some(gb_pos) = lower.find("gb") {
        let num_str: String = lower[..gb_pos]
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(gb) = num_str.parse::<f64>() {
            return gb < 8.0;
        }
    } else if lower.contains("mb") || lower.contains("kb") {
        return true;
    }
    false
}

/// Wraps a given string into multiple lines such that each line fits within `max_width`.
/// Words are separated by whitespace; words exceeding `max_width` are broken across lines.
pub fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_len = 0;

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if current_len == 0 {
            if word_len <= max_width {
                current_line.push_str(word);
                current_len = word_len;
            } else {
                let mut word_rem = word;
                while !word_rem.is_empty() {
                    let chunk: String = word_rem.chars().take(max_width).collect();
                    let chunk_len = chunk.chars().count();
                    word_rem = &word_rem[chunk.len()..];
                    if chunk_len == max_width && !word_rem.is_empty() {
                        lines.push(chunk);
                    } else {
                        current_line = chunk;
                        current_len = chunk_len;
                    }
                }
            }
        } else if current_len + 1 + word_len <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
            current_len += 1 + word_len;
        } else {
            lines.push(current_line);
            current_line = String::new();
            current_len = 0;
            if word_len <= max_width {
                current_line.push_str(word);
                current_len = word_len;
            } else {
                let mut word_rem = word;
                while !word_rem.is_empty() {
                    let chunk: String = word_rem.chars().take(max_width).collect();
                    let chunk_len = chunk.chars().count();
                    word_rem = &word_rem[chunk.len()..];
                    if chunk_len == max_width && !word_rem.is_empty() {
                        lines.push(chunk);
                    } else {
                        current_line = chunk;
                        current_len = chunk_len;
                    }
                }
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::prelude::*;
    use ratatui::widgets::{Block, Borders, Tabs};

    #[test]
    fn test_wrap_text() {
        let text = "/home [/dev/nvme0n1p2] (ext4)";
        assert_eq!(wrap_text(text, 50), vec!["/home [/dev/nvme0n1p2] (ext4)"]);
        assert_eq!(
            wrap_text(text, 20),
            vec!["/home", "[/dev/nvme0n1p2]", "(ext4)"]
        );
        assert_eq!(
            wrap_text(text, 25),
            vec!["/home [/dev/nvme0n1p2]", "(ext4)"]
        );
        assert_eq!(
            wrap_text("abcdefghijklmn", 5),
            vec!["abcde", "fghij", "klmn"]
        );
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn test_is_ram_under_8gb() {
        assert!(!is_ram_under_8gb("16GB DDR4@3200MHz"));
        assert!(!is_ram_under_8gb("32GB DDR5@5600MHz"));
        assert!(!is_ram_under_8gb("8GB DDR4@2666MHz"));
        assert!(is_ram_under_8gb("6GB DDR4"));
        assert!(is_ram_under_8gb("4GB DDR3@1333MHz"));
        assert!(is_ram_under_8gb("2GB"));
        assert!(is_ram_under_8gb("512MB"));
    }

    #[test]
    fn test_is_lts_or_latest_kernel() {
        assert!(is_lts_or_latest_kernel("7.2.0"));
        assert!(is_lts_or_latest_kernel("6.13.1-arch1"));
        assert!(is_lts_or_latest_kernel("6.12.10-lts"));
        assert!(is_lts_or_latest_kernel("6.12.10-arch1"));
        assert!(is_lts_or_latest_kernel("6.6.70-1-lts"));
        assert!(is_lts_or_latest_kernel("6.1.100-generic"));
        assert!(is_lts_or_latest_kernel("5.15.0-91-generic"));
        assert!(is_lts_or_latest_kernel("5.10.200"));
        assert!(is_lts_or_latest_kernel("linux-custom-lts"));

        // Non-LTS and older than latest (should be yellow)
        assert!(!is_lts_or_latest_kernel("6.11.2-arch1"));
        assert!(!is_lts_or_latest_kernel("6.10.14-200.fc40.x86_64"));
        assert!(!is_lts_or_latest_kernel("6.8.0-45-generic"));
        assert!(!is_lts_or_latest_kernel("6.5.0-28-generic"));
        assert!(!is_lts_or_latest_kernel("5.19.0-50-generic"));
    }

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
