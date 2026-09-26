# 🚀 rtosu-dataprovider

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust: 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)
[![Platform: Windows](https://img.shields.io/badge/Platform-Windows-blue.svg)](https://www.microsoft.com/windows)
[![CI](https://github.com/Raregendary/rtosu-dataprovider/actions/workflows/ci.yml/badge.svg)](https://github.com/Raregendary/rtosu-dataprovider/actions/workflows/ci.yml)

A lightweight native Rust data provider emitting **[tosu](https://github.com/KotRikD/tosu)**-compatible `v2` JSON and WebSocket feeds for osu!.

> ⚠️ **Note**: This is **strictly a data provider**, not a full tosu client replacement. It does not include built-in browser overlays, in-game overlay rendering, or stream elements. It is built for developers and users who need raw, low-overhead memory data without the bloatware of a full Electron application.

---

## ⚡ Overview

- **Low Memory Footprint**: Runs at **~10 MB RAM** (compared to 300 MB–500 MB in tosu).
- **Low CPU Overhead**: Uses **~3x–5x less CPU** than tosu during active gameplay.
- **tosu v2 Compatible**: Serves the exact tosu v2 JSON payload on `GET /json/v2` and broadcasts 60 Hz real-time state updates over `WS /websocket/v2`.
- **Solo & Tournament Support**: Supports both standard gameplay and multi-client tournament setups (3v3, 4v4, etc.) with automatic team splitting (`left` / `right`), score aggregation, and `#multiplayer` chat extraction.
- **Modern Performance Calculation**: Built-in gradual PP calculation powered by `rosu-pp` with modern Combo Scaling Removal (CSR) rework support.

---

## ✨ Features

- **tosu v2 REST & WebSocket Server**:
  - `GET /json/v2` — Standard tosu v2 JSON state snapshot.
  - `GET /json/v2/precise` — High-precision JSON endpoint.
  - `WS /websocket/v2` — 60 Hz real-time WebSocket state stream.
  - `GET /health` — Service health check.
- **Solo Gameplay State**:
  - Live score, accuracy, current combo, max combo, HP, and smooth HP bar.
  - Hit counts: 300, 100, 50, misses, geki, katu.
  - Mod decoding via XOR formula with ScoreV2 bitflag support.
  - Real-time grade calculation (`SS`, `S`, `A`, `B`, `C`, `D`).
  - Beatmap metadata parsing: uninherited BPM min/max/common, slider end durations, source, tags.
  - Results screen score, combo, hit distribution, and accuracy tables.
- **Tournament Manager & Spectator Client Feeds**:
  - Automatic detection of tournament client processes via 32-bit PEB inspection.
  - Auto-sorting by `ipc_id` (0..n) and team assignment (`left` / `right`).
  - Team score aggregation, star counts, and match status.
  - `#multiplayer` tournament chat extraction with team attribution.
- **Performance Engine**:
  - Built-in PP calculation powered by `rosu-pp`.
  - 10-object stepping gradual PP calculation.
  - Precalculated 90%–100% accuracy table and strain graph generation.
- **Production Architecture**:
  - **Zero-Port Bypass**: Disable HTTP or WebSocket independently; if both are disabled, no TCP socket is bound.
  - **Structured Logging**: `tracing`-based structured logging respecting configured level with daily rolling file logging (`logs/rtosu-YYYY-MM-DD.log`).
  - **Log Retention**: Automatic log pruning keeping the newest `max_log_files` (default: 7) days of logs.
  - **Graceful Shutdown**: Listens for `Ctrl+C` termination signal, draining connections and exiting cleanly.

---

## 🛠️ Getting Started

### Prerequisites
- Windows 10 / 11
- [Rust toolchain](https://www.rust-lang.org/tools/install) (Edition 2024, Rust 1.85+)

### Building from Source
```powershell
# Clone the repository
git clone https://github.com/Raregendary/rtosu-dataprovider.git
cd rtosu-dataprovider

# Build the release binary
cargo build --release
```

### Running the Server
```powershell
# Run on default port 24050 (drop-in tosu port)
cargo run --release -- serve --port 24050

# Or run with a custom config file
cargo run --release -- --config ./my-config.toml serve
```

---

## ⚙️ Configuration (`config.toml`)

`rtosu-dataprovider` is configured via `config.toml` in the working directory (or specified via `--config <path>`).

```toml
[server]
host = "127.0.0.1"        # Bind host address
port = 24050              # Drop-in tosu port (1024-65535)
cors_allow_all = true     # Permissive CORS headers for browser overlays
enable_websocket = true   # Mount /websocket/v2 stream
enable_http = true        # Mount /json/v2 and /health REST endpoints

[poll]
poll_rate_hz = 60         # 60 Hz = ~16.6ms update interval (1-1000 Hz)
scan_budget_mb = 128      # Memory signature scan limit (16-1024 MB)
default_profile = "tournament"
auto_mode = true          # Auto-detect tournament vs single-player mode

[features]
enable_chat = true        # Attributed multiplayer tournament chat
enable_pp = true          # Real-time gradual PP calculation
gradual_pp_chunks = 100   # Number of gradual PP checkpoints per beatmap (1-250)
enable_hit_errors = true  # Include full hit error array in JSON packet

[logging]
level = "info"            # "trace", "debug", "info", "warn", "error"
log_to_file = true        # Save logs to daily rolling files
max_log_files = 7         # Maximum daily log files to retain before pruning
```

---

## 💻 CLI Commands

```powershell
# Run the HTTP & WebSocket data provider (default command)
rtosu-dataprovider serve --port 24050

# View, initialize, or validate configuration
rtosu-dataprovider config show
rtosu-dataprovider config init [path]
rtosu-dataprovider config validate

# Side-by-side comparison against an active tosu server
rtosu-dataprovider compare-tosu --url http://127.0.0.1:24050/json/v2

# Stream live tournament match data in the console
rtosu-dataprovider tournament-watch --interval-ms 100

# Dump a single tournament snapshot to stdout
rtosu-dataprovider tournament-poll

# List detected osu! processes and arguments
rtosu-dataprovider processes
```

---

## 📦 Using as a Rust Library Crate

`rtosu-dataprovider` can be imported directly into other Rust applications (like native tournament overlays, analytics engines, or bots). When used as a library, **no HTTP or WebSocket server is started**—it reads memory directly in-process with minimal CPU overhead (~0.1%) and ~6-10 MB RAM footprint.

### Cargo.toml
```toml
[dependencies]
rtosu-dataprovider = { git = "https://github.com/Raregendary/rtosu-dataprovider" }
```

### High-Level API (`OsuReader`)
The `OsuReader` handles automatic solo vs. tournament detection, process lifecycle management (attaching and hot-reconnecting on game restarts), and returning complete `TosuV2Packet` snapshots:

```rust
use rtosu_dataprovider::OsuReader;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    // Zero-config builder with automatic solo vs tournament detection
    let mut reader = OsuReader::builder()
        .poll_interval(Duration::from_millis(16)) // 60 Hz polling
        .build()?;

    loop {
        let packet = reader.poll()?;
        if packet.is_tournament() {
            println!("Tournament match: {} clients connected", packet.tourney.clients.len());
        } else {
            println!("Solo play: {} - {} [{}] | Score: {} | Live PP: {:.2}",
                packet.beatmap.artist,
                packet.beatmap.title,
                packet.beatmap.version,
                packet.play.score,
                packet.pp.current
            );
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}
```

### Async Tokio Stream
For async applications, convert the reader into a `Stream`:

```rust
use rtosu_dataprovider::OsuReader;
use tokio_stream::StreamExt;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut stream = OsuReader::builder()
        .poll_interval(Duration::from_millis(16))
        .build()?
        .into_stream();

    while let Some(packet) = stream.next().await {
        // Handle live packet...
    }
    Ok(())
}
```

*(Optional: Lower-level structs like `SoloSession`, `TournamentSession`, and `ProcessMemory` remain fully accessible for custom pipelines).*

---

## 🤖 AI Development Attribution

> 💡 This library was designed, optimized, and created with the assistance of **Space Bunny Alpha** & **Gemini 3.8 Flash**.

---

## 🤝 Credits & Acknowledgements

- **[tosu](https://github.com/KotRikD/tosu)** by **KotRikD** — The original tosu tool and JSON v2 API design that this data provider implements.
- **[rosu-mem](https://github.com/486c/rosu-mem)** by **486c** — Memory reading primitives and patterns for osu! processes.
- **[rosu-pp](https://github.com/MaxOhn/rosu-pp)** and **[rosu-mods](https://github.com/MaxOhn/rosu-mods)** by **MaxOhn** — osu! difficulty and performance calculation library.
- **[rosu-pp-gemini](https://github.com/Raregendary/rosu-pp-gemini)** by **Raregendary** — Performance calculator branch supporting modern combo scaling removal mechanics.
- The **osu!** community for reverse-engineered memory structures and tournament conventions.

---

## 📄 License

This project is licensed under the **[MIT License](LICENSE)**.
