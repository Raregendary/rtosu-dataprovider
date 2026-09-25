# 🚀 rtosu-dataprovider

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust: 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)
[![Platform: Windows](https://img.shields.io/badge/Platform-Windows-blue.svg)](https://www.microsoft.com/windows)

A lightweight native Rust data provider emitting **[tosu](https://github.com/KotRikD/tosu)**-compatible `v2` JSON and WebSocket feeds for osu!.

> ⚠️ **Note**: This is **strictly a data provider**, not a full tosu client replacement. It does not include built-in browser overlays, in-game overlay rendering, or stream elements. It is built for developers and users who need raw, low-overhead memory data without the bloatware of a full Electron application.

---

## ⚡ Overview

- **Low Memory Footprint**: Runs at **~10 MB RAM** (compared to 300 MB–500 MB in tosu).
- **Low CPU Overhead**: Uses **~3x–5x less CPU** than tosu during active gameplay.
- **tosu v2 Compatible**: Serves the exact tosu v2 JSON payload on `GET /json/v2` and broadcasts 60 Hz real-time state updates over `WS /websocket/v2`.
- **Solo & Tournament Support**: Supports both standard gameplay and multi-client tournament setups (3v3, 4v4, etc.) with automatic team splitting (`left` / `right`), score aggregation, and `#multiplayer` chat extraction.
- **Modern Performance Calculation**: Optional gradual PP calculation powered by `rosu-pp` with modern Combo Scaling Removal (CSR) rework support.

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
  - Optional `pp` feature with `rosu-pp`.
  - 10-object stepping gradual PP calculation.
  - Precalculated 90%–100% accuracy table and strain graph generation.

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

# Build the release binary with PP calculation support
cargo build --release --features pp
```

### Running the Server
```powershell
# Run on default port 24050 (drop-in tosu port)
cargo run --release --features pp -- serve --port 24050

# Or run on a custom port
cargo run --release --features pp -- serve --port 24055
```

---

## 💻 CLI Commands

```powershell
# Run the HTTP & WebSocket data provider
rtosu-dataprovider serve --port 24050

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

## 📦 Using as a Rust Crate

You can embed `rtosu-dataprovider` directly into other Rust applications:

```toml
[dependencies]
rtosu-dataprovider = { git = "https://github.com/Raregendary/rtosu-dataprovider", features = ["pp"] }
```

```rust
use rtosu_dataprovider::session::SoloSession;

fn main() -> anyhow::Result<()> {
    let mut session = SoloSession::new("stable", 4, 128 * 1024 * 1024)?;

    loop {
        let packet = session.update()?;
        println!("State: {} | Song: {} | Score: {}", 
            packet.state.name, 
            packet.beatmap.title, 
            packet.play.score
        );
        std::thread::sleep(std::time::Duration::from_millis(16)); // ~60 Hz
    }
}
```

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
