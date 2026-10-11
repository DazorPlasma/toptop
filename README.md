# toptop

## Features

- **Resource Monitoring**: Real-time CPU usage, per-core breakdown, RAM & swap, network traffic, and GPU stats (AMD/NVIDIA).

- **Process Manager**: Tree view, process grouping, search/filter, and graceful signal handling (SIGTERM, SIGKILL).
- **Disk & Storage Analysis**: Filesystem usage, disk I/O metrics, and interactive directory size browser powered by `dust`.
- **Lightweight & Efficient**: Direct `/proc` and `sysfs` parsing with low CPU and memory footprint.

In Processes, **GPU eng** is the busiest DRM engine's busy-time percentage over the polling interval. The calculation uses the driver's engine capacity when supplied. It is not the same as device-wide GPU utilization in the GPU tab. GPU measurements show N/A when the driver does not expose the required counter; zero means a measured idle interval.
- **Snapshot Navigation**: Pause (`Space`) and scrub backwards/forwards through metric history.

## Installation

### With Cargo

```bash
cargo install --path .
```

### With Nix Flakes

```bash
nix run github:DazorPlasma/toptop
```

Or add to your NixOS configuration:

```nix
# flake.nix
inputs.toptop.url = "github:DazorPlasma/toptop";

# in environment.systemPackages:
inputs.toptop.packages.${pkgs.system}.default
```

## Per-process network rates

The Processes tab collects TCP payload byte rates from `ss` without sudo. Cargo installs need `ss` (from iproute2) in `PATH`. It shows rates only after a socket has appeared in two samples and its PID can be identified. The **TCP Net/s** heading means UDP, short-lived connections, inaccessible processes, and other network namespaces may be absent; `N/A` means no trustworthy rate was available. A `+` on a grouped rate means some group members were unavailable and the displayed number is only the known portion.

When toptop already runs with root privileges and `nethogs` is installed, it uses NetHogs packet capture for observed TCP and UDP rates. The Nix package includes both `ss` and `nethogs`. This mode is labeled **Net/s TCP+UDP**. Capture can still leave packets unattributed, and monitoring every interface can count tunnel traffic at more than one layer. The numbers are measured rates for attributable traffic, not a guarantee that every byte on the host has a PID. Toptop never requests sudo itself.

## Keybindings

| Key                    | Action                                                            |
| ---------------------- | ----------------------------------------------------------------- |
| `1` - `5`              | Switch tabs (General, Processes, CPU/Memory, GPU, Network, Disks) |
| `Tab` / `Shift+Tab`    | Cycle through sub-tabs / categories                               |
| `j` / `k` or `↓` / `↑` | Navigate lists and tables                                         |
| `h` / `l` or `←` / `→` | Switch sub-tabs or fold/unfold                                    |
| `Enter`                | Expand / collapse directory in Disk view or show process detail   |
| `.`                    | Toggle hidden files and folders < 4.0 KB in Disk view             |
| `Space`                | Pause / resume real-time metrics                                  |
| `q` / `Esc`            | Quit                                                              |

## License

GNU General Public License v3.0 or later (GPL-3.0-or-later). See [LICENSE](LICENSE) for details.
