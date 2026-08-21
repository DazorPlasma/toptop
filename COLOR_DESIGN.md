# toptop Color Coding & UI Palette Reference

This document summarizes all visual styling, RGB gradient profiles, and color-coding decisions implemented across **toptop**.

---

## 1. Global Multi-Stop Metric Gradient Profile

All live charts, core gauges, memory usage bars, and process metrics share a unified, continuous RGB gradient:

| Value Range | Color Name | RGB Value | Hex Code | Visual Meaning |
| :--- | :--- | :--- | :--- | :--- |
| **`0%` (Procs)** | Faint Dark Green | `RGB(0, 85, 0)` | `#005500` | Inactive / Zero usage baseline |
| **`0% – 5%`** | Dark Green | `RGB(0, 130, 0)` $\to$ `RGB(0, 155, 0)` | `#008200` $\to$ `#009B00` | Minimal / Idle activity |
| **`5% – 20%`** | Dark Green $\to$ Pure Green | `RGB(0, 155, 0)` $\to$ `RGB(0, 255, 0)` | `#009B00` $\to$ `#00FF00` | Normal low load |
| **`20% – 55%`** | Green $\to$ Yellow | `RGB(0, 255, 0)` $\to$ `RGB(255, 255, 0)` | `#00FF00` $\to$ `#FFFF00` | Moderate activity |
| **`55% – 85%`** | Yellow $\to$ Orange | `RGB(255, 255, 0)` $\to$ `RGB(255, 128, 0)` | `#FFFF00` $\to$ `#FF8000` | Elevated load |
| **`85% – 100%`** | Orange $\to$ Pure Red | `RGB(255, 128, 0)` $\to$ `RGB(255, 0, 0)` | `#FF8000` $\to$ `#FF0000` | Heavy load / High saturation |
| **`> 80% Total Core Capacity`** | Red $\to$ Rich Violet | `RGB(255, 0, 0)` $\to$ `RGB(180, 0, 255)` | `#FF0000` $\to$ `#B400FF` | Multi-core compute saturation |

---

## 2. Metric Mapping Scales

| Metric Column | Value Used for Color & Sorting | Color Function |
| :--- | :--- | :--- |
| **`CPU`** | Process CPU % (supports $>100\%$ multi-core) | `process_cpu_color(pct, num_cores)` (Green $\to$ Yellow $\to$ Red $\to$ Violet) |
| **`RAM`** | Process RSS as % of System Total RAM $\times 2$ | `gradient_color(ram_pct * 2.0)` |
| **`GPU`** | Process GPU % ($0\dots 100\%$) | `gradient_color(gpu_pct)` |
| **`VRAM`** | Process VRAM as % of 8 GB baseline $\times 2$ | `gradient_color(vram_pct * 2.0)` |
| **`IO`** | $\max(\text{Read Speed}, \text{Write Speed})$ | `gradient_color(io_gradient_pct(max_io))` ($0\dots 50+\text{ MB/s}$), formatted with `↑` (Read) / `↓` (Write) |
| **`Net`** | $\max(\text{Download Speed}, \text{Upload Speed})$ | `gradient_color(io_gradient_pct(max_net))` ($0\dots 50+\text{ MB/s}$), formatted with `↓` (Down) / `↑` (Up) |

---

## 3. Process Table Typography & Selection State

- **Highlighted / Selected Row**:
  - **Background**: Dark Gray Fill (`RGB(45, 45, 45)` / `#2D2D2D`).
  - **Text Identifiers** (`PID`, `User`, `Name`, `State`, `Threads`): Crisp Pure White (`RGB(255, 255, 255)` / `#FFFFFF`).
  - **Metrics** (`CPU`, `RAM`, `GPU`, `VRAM`, `IO`, `Net`): Full Vibrant Brightness (`100%` brightness).
  - **Zero Values**: Faint Dark Green (`RGB(0, 85, 0)` / `#005500`).

- **Unhighlighted Rows**:
  - **Background**: Transparent / Default terminal background.
  - **Text Identifiers**: Darker Gray (`RGB(130, 130, 130)` / `#828282`).
  - **Metrics**: Dimmed Gradient (`55%` brightness via `darken_color(c, 0.55)`).
  - **Zero Values**: Very Faint Dark Green (`RGB(0, 47, 0)` / `#002F00`).

- **Sorted Column Indicator**:
  - Bold Yellow (`RGB(255, 255, 0)` / `#FFFF00` with `▲` / `▼` sort arrow).

---

## 4. General Tab (Tab 1) - Complete System Overview Dashboard

- **System Overview & Uptime Header Card**:
  - **Host & OS**: Hostname and OS distribution name (`os-release`).
  - **Kernel & Uptime**: Linux kernel version and live formatted uptime (`format_uptime`).
  - **Desktop & Shell**: Active Desktop Environment / Window Manager (e.g. `mango (Wayland)`, `KDE Plasma (Wayland)`, `GNOME (X11)`) and active shell (e.g. `fish`, `zsh`, `bash`).
  - **CPU**: Processor model name and core count (e.g. `AMD Ryzen 5 3600X 6-Core Processor`).
  - **GPU**: GPU model name and total VRAM size (e.g. `AMD Radeon RX 7600 (8GB)`).
  - **RAM**: DMI memory capacity, speed, and DDR type (e.g. `16GB DDR4@3200MHz`).
  - **Network & Locale**: Local outbound IP address (e.g. `192.168.1.150`) and system locale (e.g. `en_US.UTF-8`).
  - **Displays**: Native DRM/EDID monitor detection parsing monitor model name, resolution, screen diagonal in inches, maximum refresh rate (Hz), and connector type (e.g. `Display:  (AW2724DM) 2560x1440 in 27", 180 Hz [External]`).
  - **Load Averages**: $1\text{m}$, $5\text{m}$, $15\text{m}$ system load averages.
  - **Copy Overview Button**: Bottom-right interactive button (`[ 'c' Copy ]` $\to$ `[ ✓ Copied! ]`) to copy all telemetry text to clipboard via `wl-copy`, `xclip`, `xsel`, or OSC 52 escape sequences on keypress <kbd>c</kbd> or mouse click.
- **Battery & Power Card**:
  - **Battery Present (Laptops)**: Dynamic multi-color horizontal gradient capacity gauge (`█`/`─`), percentage readout, live **Drainage Rate** (e.g. `-14.2 W (Drainage)`) or **Charging Rate** (e.g. `+45.0 W (Charging)`), battery health status, and stored/capacity energy in Wh.
  - **Desktop / AC Power (No Battery)**: Active status indicator (`"Power Source: AC Connected (Desktop System / No Battery)"`) in vibrant emerald green (`RGB(0, 255, 128)`).
- **CPU Performance Card**:
  - Aggregate CPU load gradient meter with **color-coded live clock speed** (gradient scaled across minimum base clock and maximum boost clock), **color-coded temperature reading** ($25^\circ\text{C}\dots 100^\circ\text{C}$), and CPU model identifier.
  - 2-column per-core mini gradient meters (`C0`, `C1`, etc.) displaying core-by-core load in real time.
- **Memory & Swap Card**:
  - RAM usage gradient bar (`X GB / Y GB` with percentage and DMI hardware memory frequency/type).
  - Swap partition usage gradient bar (`X GB / Y GB` with percentage).
- **High Resource Processes (>30%) Card**:
  - Automatically filters and highlights all processes currently consuming $\ge 30\%$ of CPU, RAM, GPU Core, or GPU VRAM.
  - Columns: `PID`, `NAME`, `CPU%`, `MEM%`, `GPU%`, `VRAM%`, and `USER` with multi-stop gradient styling for high-usage metrics.
  - Fallback status: `"No processes exceeding 30% resource threshold"` when the system is idle.
- **GPU & VRAM Card**:
  - GPU Core utilization gradient bar with **color-coded clock speed** (gradient scaled to max boost clock), **color-coded temperature** ($25^\circ\text{C}\dots 100^\circ\text{C}$), and GPU device name.
  - GPU VRAM consumption gradient meter.
- **Network Throughput Card**:
  - Real-time **Download (RX)** (`↓/s`) dynamic gradient throughput meter.
  - Real-time **Upload (TX)** (`↑/s`) dynamic gradient throughput meter.
- **Disk Throughput Card**:
  - Real-time **Disk Read (↑)** (`↑/s`) dynamic gradient activity meter.
  - Real-time **Disk Write (↓)** (`↓/s`) dynamic gradient activity meter.
- **Storage & Partitions Card**:
  - Partition mount point usage gradient bars (`/`, `/boot`, `/home`, etc.) with used/total GB and percentage fill.

---

## 5. Processes Tab (Tab 2)

- **Tab Bar (Top)**:
  - **Active Tab**: Bold Pure White (`RGB(255, 255, 255)`, `.bold()`).
  - **Inactive Tab**: Unbold Off-White / Gray (`RGB(170, 170, 170)`, `.not_bold()`).
  - **Pause Badge & Outline**: When paused, the topbar outline and "System Monitor" title text are highlighted in Bold Yellow (`RGB(255, 255, 0)` / `#FFFF00`).

- **Window Box Outlines**:
  - Thin single-line borders (`BorderType::Plain`) styled with subtle Dark Gray (`RGB(60, 60, 60)` / `#3C3C3C`).

---

## 6. CPU & RAM Tab (Tab 3)

- **CPU Section**:
  - **CPU History Graph**: Braille canvas plotting global CPU history.
  - **Top-Center**: Bold gradient clock speed readout.
  - **Top-Right**: CPU model name.
  - **Cores Gauge**: Individual per-core usage meters (`C0..Cn`).
  - **Temp Gauge**: Vertical thermometer barchart.
- **Memory & Swap Section**:
  - **Memory History Graph**: Braille canvas plotting RAM usage history with top-right DDR spec info.
  - **Memory Usage Bar**: Full-width gradient fill meter displaying `used_mem MB / total_mem MB` with inverted contrast centered text.
  - **Swap Usage Bar**: Full-width gradient fill meter positioned directly below Memory Usage displaying `used_swap MB / total_swap MB`.

---

## 7. GPU Tab (Tab 4)

- **GPU Utilization History**:
  - Full-width Braille gradient chart plotting live GPU core load ($0\dots 100\%$).
  - **Top-Left**: `"GPU Utilization History"`
  - **Top-Center**: Current GPU clock speed (e.g. `" 972 MHz "`), styled in **bold gradient color** where Green is the GPU base/idle clock and Red is the maximum boost clock.
  - **Top-Right**: GPU model with total VRAM (e.g. `" AMD Radeon RX 7600 (8GB) "`) in unbold gray (`#AAAAAA`).
- **Vertical Temperature Barchart**:
  - Slim right container titled `"Temp"` with centered real-time temperature readout (e.g. ` 65°C `) and a vertical multi-color gradient thermal column (`█`) filling from bottom to top based on current thermal level ($25^\circ\text{C}\dots 100^\circ\text{C}$), with empty track styled in dark gray (`░`).
- **VRAM History & Usage Meter**:
  - VRAM History continuous Braille gradient graph with live top-right VRAM ratio indicator (e.g. `" 1508 MB / 8176 MB "`).
  - VRAM Usage gradient fill bar with overlaid `X MB / Y MB` centering label.

---

## 6. Network Tab (Tab 4)

- **Download (RX) Speed History**:
  - Continuous Braille gradient chart plotting live incoming bandwidth with live right-aligned download rate (`X MB/s ↓`).
- **Upload (TX) Speed History**:
  - Continuous Braille gradient chart plotting live outgoing bandwidth with live right-aligned upload rate (`Y MB/s ↑`).
- **Primary Interface Box**:
  - Active adapter status, link speed, MAC address, and cumulative transferred/received byte counters.
- **All Interfaces Overview**:
  - Live summary of all detected network devices and their real-time bidirectional speeds.

---

## 7. Disks Tab (Tab 5)

- **Disk Read (↑) History**:
  - Continuous Braille gradient chart plotting aggregate disk read throughput with live right-aligned read rate (`X MB/s ↑`).
- **Disk Write (↓) History**:
  - Continuous Braille gradient chart plotting aggregate disk write throughput with live right-aligned write rate (`Y MB/s ↓`).
- **Physical Disks Box**:
  - Lists detected physical block drives (e.g. `nvme0n1`, `sda`, `sdb`), drive model names, and per-drive real-time read/write throughput.
- **Mounted Filesystems & Usage Bars**:
  - Overview of mounted volumes (`/`, `/boot`, `/home`, etc.) with partition device identifiers, filesystem types, available free space, and colored gradient percentage fill bars.



