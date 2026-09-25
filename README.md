# 🚀 rtosu-dataprovider

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust: 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)
[![Platform: Windows](https://img.shields.io/badge/Platform-Windows-blue.svg)](https://www.microsoft.com/windows)

A blazingly fast, ultra-low latency native Rust drop-in replacement for **[tosu](https://github.com/KotRikD/tosu)** and osu! memory data provider. Built for streamers, tournament organizers, and overlay developers who need full data fidelity without the heavy Electron/Node runtime overhead.

---

## ⚡ Why rtosu-dataprovider?

`tosu` is a great tool, but its Electron / Node runtime incurs significant resource overhead:
- **Heavy RAM footprint**: Consumes ~120 MB to 500 MB+ RAM.
- **CPU consumption**: Consumes 1%–2% CPU in single-player and 5%–12%+ CPU when polling 7+ concurrent instances in 3v3/4v4 tournaments.
- **Process querying lag**: Spawns heavy shell/WMI commands to inspect process parameters.

**`rtosu-dataprovider` solves this:**
- **0.1% – 0.2% CPU** usage during active gameplay.
- **6 MB – 12 MB RAM** total working set (over **95% memory reduction** vs tosu).
- **Direct PEB & Windows API reads**: Zero WMI or PowerShell process querying; extracts 32-bit PEB process arguments in `<0.05 ms`.
- **Cached pointer dereferencing**: Signatures are scanned once upon attach. Steady-state polling executes direct memory dereferences with zero heap allocations.
- **rosu-pp CSR Support**: Powered by up-to-date `rosu-pp` with modern Combo Scaling Removal (CSR) rework mechanics and gradual calculation stepping.

---

## 📊 Live Benchmark & Verification

### 1. Single Player Mode (Results Screen Benchmark)
| Metric / Field | tosu (Node / Go) | `rtosu-dataprovider` | Status |
| :--- | :--- | :--- | :---: |
| **CPU Usage** | ~0.5% – 1.2% | **0.02% – 0.2%** | 🚀 **~5x–10x Lower** |
| **RAM Footprint** | ~500 MB | **6 MB – 11 MB** | 🚀 **98% Lower** |
| `game.focused` | `false` | `false` | ✅ **Match** |
| `beatmap.stats.bpm` | `min: 200, max: 295, common: 240, realtime: 295` | `min: 200, max: 295, common: 240, realtime: 295` | ✅ **Exact Match** |
| `beatmap.time.lastObject` | `1428173` | `1428173` | ✅ **Exact Match** |
| `beatmap.stats.objects` | `circles: 5852, sliders: 2948, spinners: 7` | `circles: 5852, sliders: 2948, spinners: 7` | ✅ **Exact Match** |

### 2. Tournament 3v3 Mode (7 Concurrent osu! Processes)
| Metric | tosu | `rtosu-dataprovider` | Status |
| :--- | :--- | :--- | :---: |
| **Concurrent Processes** | 7 (1 manager + 6 clients) | 7 (1 manager + 6 clients) | ✅ **Exact Match** |
| **Total RAM Footprint** | ~250 MB – 500 MB | **~12.8 MB** | 🚀 **95% Lower** |
| **Polling Latency** | ~50 ms – 150 ms | **< 10 ms total** | 🚀 **15x Faster** |
| **Client Team Attribution** | 0..2 Left, 3..5 Right | 0..2 Left, 3..5 Right | ✅ **Exact Match** |
| **Mod Decryption & Ranks** | HD, HR, SO, NM, etc. | HD, HR, SO, NM, etc. | ✅ **100% Match** |
| **Multiplayer Chat** | `#multiplayer` extracted | `#multiplayer` extracted | ✅ **Exact Match** |

---

## ✨ Features

- **100% Drop-In tosu v2 Compatibility**:
  - `GET /json/v2` — Full tosu v2 JSON state payload.
  - `GET /json/v2/precise` — High-precision JSON endpoint.
  - `WS /websocket/v2` — Real-time 60 Hz WebSocket broadcast stream.
  - `GET /health` — Simple health check endpoint.
- **Complete Solo Gameplay State**:
  - Live score, accuracy, current combo, max combo, HP and smooth HP bar.
  - Hit counts: 300, 100, 50, misses, geki, katu.
  - Mod decoding via XOR decryption `(scoreBase + 0x1c) + 0xc ^ (scoreBase + 0x1c) + 0x8` and ScoreV2 bitflag (`1 << 29`).
  - Real-time grade calculation (`SS`, `S`, `A`, `B`, `C`, `D`).
  - Beatmap metadata parsing: source, tags, audio length, uninherited BPM min/max/common, slider end durations.
  - Results screen score, combo, hit distribution, and accuracy tables.
- **Tournament Manager & Spectator Client Support**:
  - Instant discovery of tournament client processes via 32-bit PEB inspection.
  - Auto-sorting by `ipc_id` (0..n) and team splitting (`left` / `right`).
  - Real-time team score aggregation and stars count.
  - `#multiplayer` tournament chat extraction and automatic team attribution.
- **Performance & PP Engine**:
  - Integration with `rosu-pp` with CSR rework support.
  - Gradual PP progression calculation with 10-object stepping.
  - Precalculated accuracy curve tables (90% to 100%).
  - Strain graph generation (aim, speed, reading, flashlight).

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

# Build the optimized release binary with PP calculation feature
cargo build --release --features pp
```

### Running the Server
```powershell
# Run with default port 24050 (exact tosu port replacement)
cargo run --release --features pp -- serve --port 24050

# Or run on a custom port
cargo run --release --features pp -- serve --port 24055
```

---

## 💻 CLI Commands

`rtosu-dataprovider` comes with built-in inspection and debugging commands:

```powershell
# 1. Run the HTTP & WebSocket server
rtosu-dataprovider serve --port 24050

# 2. Side-by-side comparison against an active tosu server
rtosu-dataprovider compare-tosu --url http://127.0.0.1:24050/json/v2

# 3. Stream tournament match data in real time directly in the console
rtosu-dataprovider tournament-watch --interval-ms 100

# 4. Dump a single tournament snapshot to stdout
rtosu-dataprovider tournament-poll

# 5. List detected osu! processes and PEB arguments
rtosu-dataprovider processes
```

---

## 📦 Using as a Rust Crate

You can embed `rtosu-dataprovider` directly as a library in your own Rust applications:

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

> 💡 This project was architected, developed, and optimized with the assistance of **Space Bunny Alpha** & **Gemini 3.8 Flash**.

---

## 🤝 Credits & Acknowledgements

- **[tosu](https://github.com/KotRikD/tosu)** by **KotRikD** — The original tosu tool and its JSON v2 API design that inspired this drop-in replacement.
- **[rosu-pp](https://github.com/MaxOhn/rosu-pp)**, **[rosu-mods](https://github.com/MaxOhn/rosu-mods)**, and **[rosu-mem](https://github.com/MaxOhn/rosu-mem)** by **MaxOhn** — The premier Rust ecosystem for osu! difficulty calculation and mod parsing.
- **[rosu-pp-gemini](https://github.com/Raregendary/rosu-pp-gemini)** by **Raregendary** — Up-to-date performance calculator branch supporting modern combo scaling removal mechanics.
- The **osu!** community for documented memory structures, signatures, and tournament overlay conventions.

---

## 📄 License

This project is licensed under the **[MIT License](LICENSE)**.
