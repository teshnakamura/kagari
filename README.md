# kagari (篝)

A small, Wayland-native live graph of system metrics (temperatures, CPU/memory
usage, fan speeds), written in Rust with GTK4.

The name comes from *kagari-bi* (篝火), a watchfire kept burning through the night
to keep watch. It fits the tool on three counts: it **watches** (monitoring), it
is about **heat**, and it **never goes dark** (it keeps updating on Wayland).

## Why

`psensor` is a GTK2 application that runs under XWayland on modern Wayland
sessions. There it suffers from Mutter's frame-callback throttling: the graph
**stops updating even while the window is visible**, and only redraws when the
window receives an event (e.g. a resize).

`kagari` avoids this by design. It cleanly separates two concerns:

- **Collection timer** (`glib::timeout`) — always polls the data sources and
  appends to each metric's history, independently of drawing.
- **Drawing** (`DrawingArea` draw func) — runs only while the window is visible,
  and is requested on every collection tick.

Because it is a native GTK4 Wayland client, it redraws reliably as long as the
window is on screen.

## Features

- Live time-series graph with a configurable history window.
- Metrics grouped into separate stacked bands by unit, each with its own
  auto-scaled Y axis:
  - **Temperature (°C)** — every `tempN_input` from `lm-sensors`, plus the
    NVIDIA GPU temperature.
  - **Usage (%)** — total and per-core CPU usage (from `/proc/stat`), memory
    usage (from `/proc/meminfo`), and NVIDIA GPU fan speed.
  - **Fan (RPM)** — every `fanN_input` exposed by `lm-sensors`.
  - **Network (bytes/s)** — download (RX) and upload (TX) throughput summed over
    all interfaces except loopback (from `/proc/net/dev`).
  - **Disk I/O (bytes/s)** — read and write throughput summed over the physical
    disks (from `/sys/block/*/stat`, excluding partitions and virtual devices).
- Per-series on/off toggles in a side panel, each with a color swatch.
- Latest value shown at the right edge of each series.

> Note: fan readings only appear if your hardware exposes them. Many laptops do
> not publish fan RPM via `lm-sensors`, and NVIDIA Max-Q GPUs often report fan
> speed as `N/A`. This is a hardware/driver limitation, not a bug in the tool.

## Requirements

- Linux with GTK 4 (`libgtk-4`) and Cairo.
- `lm-sensors` (`sensors` command) for temperatures and fans.
- Optional: `nvidia-smi` for NVIDIA GPU temperature and fan speed.
- Rust toolchain (for building).

On Debian/Ubuntu the build dependencies are:

```sh
sudo apt install libgtk-4-dev lm-sensors
```

## Install on Debian / Ubuntu

Download (or build) the `.deb` and install it:

```sh
sudo apt install ./kagari_0.1.5_amd64.deb
```

`apt` resolves the runtime dependencies (`libgtk-4-1`, `lm-sensors`). After
installation, launch it from the application grid ("Kagari") or run `kagari`.

## Build

```sh
cargo build --release
```

### Build the .deb yourself

```sh
./scripts/build-deb.sh        # writes dist/kagari_<version>_<arch>.deb
```

This drives `dpkg-deb` directly, so it works on any Rust toolchain.

## Run

```sh
./target/release/kagari
```

Set `KAGARI_DEBUG=1` to print polling activity to stderr.

## Configuration

Tunables are constants at the top of `src/main.rs`:

| Constant             | Meaning                          | Default          |
| -------------------- | -------------------------------- | ---------------- |
| `POLL_INTERVAL_SECS` | Sampling interval                | `2` seconds      |
| `HISTORY_LEN`        | Number of points kept per series | `600` (= 20 min) |
| `WINDOW_W` / `WINDOW_H` | Initial window size           | `1100` x `620`   |
| `SIDE_PANEL_W`       | Toggle panel width               | `200`            |

State remembered across runs (under `$XDG_CONFIG_HOME/kagari/`, default
`~/.config/kagari/`):

- `visibility.json` — the per-series on/off state from the side panel.
- `window.json` — the last window size.

## License

[MIT](LICENSE) © 2026 Tesh Nakamura
