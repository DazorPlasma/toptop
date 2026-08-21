# toptop

## Features

- **Resource Monitoring**: Real-time CPU usage, per-core breakdown, RAM & swap, network traffic, and GPU stats (AMD/NVIDIA).
- **Process Manager**: Tree view, process grouping, search/filter, and graceful signal handling (SIGTERM, SIGKILL).
- **Disk & Storage Analysis**: Filesystem usage, disk I/O metrics, and interactive directory size browser powered by `dust`.
- **Lightweight & Efficient**: Direct `/proc` and `sysfs` parsing with low CPU and memory footprint.
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
