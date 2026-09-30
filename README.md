# 🚀 rtosu-dataprovider

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust: 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)
[![Platform: Windows](https://img.shields.io/badge/Platform-Windows-blue.svg)](https://www.microsoft.com/windows)
[![CI](https://github.com/Raregendary/rtosu-dataprovider/actions/workflows/ci.yml/badge.svg)](https://github.com/Raregendary/rtosu-dataprovider/actions/workflows/ci.yml)

A lightweight native Rust data provider emitting **[tosu](https://github.com/KotRikD/tosu)**-compatible `v2` JSON and WebSocket feeds for osu!.

> ⚠️ **Note**: This is primarily a **data provider**, not a full tosu client replacement. It does not bundle its own overlay designs — instead it serves **any** tosu v2 compatible browser overlay you drop into `browser_overlays/`. It also does not include in-game overlay rendering or stream elements. It is built for developers and users who need raw, low-overhead memory data without the bloatware of a full Electron application.

---

## ⚡ Overview

- **Low Memory Footprint**: Runs at **~10 MB RAM** (compared to 300 MB–500 MB in tosu).
- **Low CPU Overhead**: Uses **~3x–5x less CPU** than tosu during active gameplay.
- **tosu v2 Compatible**: Serves the exact tosu v2 JSON payload on `GET /json/v2` and broadcasts 60 Hz real-time state updates over `WS /websocket/v2`.
- **Solo & Tournament Support**: Supports both standard gameplay and multi-client tournament setups (3v3, 4v4, etc.) with automatic team splitting (`left` / `right`), score aggregation, and `#multiplayer` chat extraction.
- **Modern Performance Calculation**: Built-in gradual PP calculation powered by `rosu-pp` with modern Combo Scaling Removal (CSR) rework support.
- **Browser Overlay Hosting**: Serves any tosu v2 compatible overlay dropped into `browser_overlays/`, with a dashboard to browse them and a compatibility shim so drop-in overlays work unmodified.
- **Documented Release History**: Every release from v1.0.0 onward is described in [`CHANGELOG.md`](CHANGELOG.md), including the deliberate differences from tosu.

---

## ✨ Features

- **tosu-compatible REST & WebSocket Server**, on tosu's own paths
  (`packages/server/router/index.ts`):
  - `GET /json` — Gosumemory-compatible **v1** payload, which is what tosu serves here. Set `server.json_payload = "v2"` to put rtosu's older v2 payload back on this path.
  - `GET /json/v1` — The same v1 payload, as an explicit alias.
  - `GET /json/v2` — Standard tosu v2 JSON state snapshot.
  - `GET /json/v2/precise` — `{keys, hitErrors, tourney}` and nothing else: the high-frequency feed, without the strain graph.
  - `GET /json/sc` — StreamCompanion-compatible payload (136 flat keys).
  - `WS /websocket/v2` — 60 Hz real-time v2 state stream.
  - `WS /websocket/v2/precise` — The precise payload, streamed.
  - `WS /ws` — The v1 payload, streamed (tosu's `WS_V1`).
  - `WS /tokens` — StreamCompanion payloads over a socket, with filter support (tosu's `WS_SC`).
  - `WS /websocket/commands` — Inbound-only command channel (tosu's `WS_COMMANDS`).
  - `GET /health` — Service health check.
  - `GET /` — Settings landing page: the live configuration in a form next to your overlays.
  - `GET /api/settings`, `POST /api/settings` — The landing page's JSON API (read the config; apply a validated patch).
  - `GET /files/beatmap/background` — Current beatmap background, for overlays.
  - `GET /files/beatmap/{*path}`, `GET /Songs/{*path}` — The osu! songs folder, under either path, for overlays that load audio or images by file.
  - `GET /files/skin/{*path}` — The skin folder.
  - `GET /backgroundImage` — Alias of the background route, for older overlays.

  With no osu! attached, all four `/json*` routes answer `500
  {"error":"osu is not ready/running"}` and the sockets send nothing, which is
  what tosu does (`packages/server/utils/http.ts`).
- **Browser Overlays**:
  - `GET /overlays` — Dashboard listing every overlay found in `browser_overlays/`.
  - `GET /overlays/<folder>/` — The overlay itself; paste this URL into an OBS **Browser** source.
  - Automatic shim injection rewrites tosu API calls (`ws://127.0.0.1:24050/ws`, `/websocket/v2`, `/backgroundImage`, `/Songs/...`) onto the page's own origin, so drop-in overlays need no edits.
  - Live directory re-scan: drop a folder in mid-session and it appears without a restart.
- **Settings Landing Page** (`http://127.0.0.1:24050/`):
  - Every `config.toml` setting in one form, next to the overlay cards, for machines with no dashboard app.
  - Saving patches `config.toml` in place: comments, key order and every line you did not change survive byte for byte.
  - `[features]`, `[scoring]` and `poll.poll_rate_hz` take effect on the next poll; the rest is saved and marked *restart* in the page.
  - Writes are restricted to the local machine by default (`server.settings_write_local_only`); everyone else gets a read-only view.
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
  - Optional **per-mod score weighting** (`[scoring]`): rate a play by the mods it was set on, with `tourney.totalScore` recomputed from the weighted per-player scores.
  - `#multiplayer` tournament chat extraction with team attribution.
- **Performance Engine**:
  - Built-in PP calculation powered by `rosu-pp`.
  - 10-object stepping gradual PP calculation.
  - Precalculated 90%–100% accuracy table and strain graph generation.
  - Optional NoFail-exempt PP (`features.ignore_nf_for_pp`), for tournaments that force NF on the whole lobby.
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

### Browser Overlays

Put each tosu v2 compatible overlay in its own folder under `browser_overlays/`
(a folder counts as an overlay as soon as it contains an `index.html`), then
open the dashboard:

```
http://127.0.0.1:24050/overlays/
```

Copy a card's URL into an OBS **Browser** source. Drop-in overlays that hardcode
a tosu address keep working — the server injects a compatibility shim into each
page that redirects those calls back to itself. See
[`browser_overlays/README.md`](browser_overlays/README.md) for details.

### Settings Landing Page

Open the root of the server in a browser:

```
http://127.0.0.1:24050/
```

The page shows every setting in `config.toml` as a form, grouped by section,
next to the same overlay cards the `/overlays` dashboard renders. **Save
changes** writes the config file back in place — comments, key order and every
line you did not touch are preserved exactly — and applies what can be applied
without a restart. A setting marked *restart* is saved immediately but only read
at startup, and the page says so rather than pretending otherwise.

| Path | What it does |
| --- | --- |
| `GET /` | The page. |
| `GET /api/settings` | The current config plus the field table, and whether this viewer may write. |
| `POST /api/settings` | A flat patch of `section.key` values, e.g. `{"poll.poll_rate_hz": 120}`. Validated against the same rules the config file is loaded with; on any error nothing is applied and nothing is written. |

**Who may write.** `server.settings_write_local_only = true` (the default)
accepts settings writes only from the loopback interface, and only from a
request with no `Origin` header or a loopback one. Readers on the LAN still see
the page and the JSON; they cannot change anything. This is a convenience guard,
**not authentication** — the real boundary is `host = "127.0.0.1"`, so do not
publish port 24050 beyond a network you trust.

Applied live, no restart: `[features]`, `[scoring]`, `poll.poll_rate_hz`.
Needs a restart: everything under `[server]`, `poll.scan_budget_mb`,
`poll.default_profile`, `poll.auto_mode`, `[logging]`.

tosu's own `POST /api/settingsSave` is deliberately not implemented: its body is
an Electron dashboard record with a different schema, and serving the same path
with an incompatible body would be worse than not having it.

---

## ⚙️ Configuration (`config.toml`)

`rtosu-dataprovider` is configured via `config.toml` in the working directory (or specified via `--config <path>`).

```toml
[server]
host = "127.0.0.1"        # Bind host address
port = 24050              # Drop-in tosu port (1024-65535)
cors_allow_all = true     # Permissive CORS headers for browser overlays
enable_websocket = true   # Mount /websocket/v2 stream
enable_http = true        # Mount the /json endpoints, /health and the settings page
json_payload = "v1"       # What GET /json serves: "v1" (tosu's choice) or "v2"
enable_overlays = true    # Serve overlays from overlays_dir (needs enable_http)
overlays_dir = "browser_overlays"  # One subfolder per overlay, each with index.html
settings_write_local_only = true   # Only the local machine may save settings (GET / is read-only for everyone else)

[poll]
poll_rate_hz = 60         # 60 Hz = ~16.6ms update interval (1-120 Hz)
scan_budget_mb = 128      # Memory signature scan limit (16-1024 MB)
default_profile = "tournament"
auto_mode = true          # Auto-detect tournament vs single-player mode

[features]
enable_chat = true        # Attributed multiplayer tournament chat
enable_pp = true          # Real-time gradual PP calculation
ignore_nf_for_pp = false  # Rate NF plays as if NoFail were not on them
enable_hit_errors = true  # Include full hit error array in JSON packet

[scoring]
enable_mod_multipliers = false  # Weight reported scores by mods (off: in-game scores)
mod_multipliers = { "NM" = 1.0, "NF" = 1.0, "EZ" = 1.0, ... }  # Every mod, at 1.0 by default

[logging]
level = "info"            # "trace", "debug", "info", "warn", "error"
log_to_file = true        # Save logs to daily rolling files
max_log_files = 7         # Maximum daily log files to retain before pruning
```

### Per-Mod Score Multipliers

`[scoring] mod_multipliers` weights the score a play reports, for tournaments
that rate a play by the mods it was set on. It ships listing **every** mod at
`1.0`, so the in-game score is what you get until you change a value:

```toml
[scoring]
enable_mod_multipliers = true
mod_multipliers = { "NM" = 1.0, "NF" = 0.5, "EZ" = 1.8, "HD" = 1.05, "HR" = 1.1, "DT/NC" = 1.1 }
```

* While enabled, `play.score`, `resultsScreen.score` and every
  `tourney.clients[].play.score` are multiplied, and `tourney.totalScore` becomes
  the sum of those weighted scores — the tournament manager's own total is a sum
  of *unweighted* scores and cannot be rescaled once one client carries a
  different factor than another. Tournament overlays should read
  `tourney.totalScore` rather than adding up client scores themselves.
* Accuracy, rank, pp, `profile.*` and `leaderboard[].score` are never weighted.
* Factors of different mods **multiply**: HDHR with `{ "HD" = 1.05, "HR" = 1.1 }`
  scores 1.155x.
* Keys that name more than one acronym are the mods osu! sets in a single slot
  (Nightcore sets the DoubleTime bit too), so `"DT/NC"` is one factor rather than
  two; writing `"DT"` or `"NC"` alone sets the same slot. The other such slots
  are `"SD/PF"` and `"AT/CN"`. `"NM"` is a play with no mods.
* Keys are case-insensitive, and an unknown acronym is a **config error** rather
  than a silently ignored entry — `rtosu-dataprovider config validate` names the
  offending key. Factors must be between `0.01` and `100.0`.

### Rating NoFail Plays Without the NoFail Penalty

`features.ignore_nf_for_pp` (default `false`) computes PP as if the NoFail mod
were not on the play, for tournaments that force NF on every player:

```toml
[features]
enable_pp = true
ignore_nf_for_pp = true
```

* `play.mods` still reports NF, and star rating, accuracy, hits, combo and rank
  are untouched — only the pp family moves: `play.pp`, `resultsScreen.pp`,
  `beatmap.stats.pp`, and the pp served for each `tourney.clients[]`.
* How much comes back depends on the ruleset and the scoreline, because that is
  what osu! takes away. osu!mania pays a flat **×0.75**; osu!standard and
  osu!catch pay `(1 - 0.02 × misses)` floored at `0.9`, so a **missless FC is
  unchanged** and a play with five or more misses gains about 11 %; osu!taiko is
  a no-op, because the calculator applies no NF penalty there.
* The reported value is therefore *not* what osu! would submit, and not what the
  osu! website shows — that is the point of the setting.

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

### Weighting a Play by Its Mods

The library path takes the same table `[scoring]` does, so a consumer can weight
scores without a config file. `identity()` means "report the in-game score":

```rust
use rtosu_dataprovider::{OsuReader, scoring::ModMultipliers};

let table = std::collections::HashMap::from([
    ("EZ".to_string(), 1.8),
    ("NF".to_string(), 0.5),
    ("NM".to_string(), 1.0),
]);
let mut reader = OsuReader::builder()
    .mod_multipliers(ModMultipliers::new(&table)?)
    .build()?;
```

Keys are validated by `ModMultipliers::new` (`"DT/NC"` names one osu! slot, an
unknown acronym is an error), and the weighted score is `round(score x factor)`
saturated to `i32::MAX`.

The pp toggle is on the builder too, for the same reason:

```rust
let mut reader = OsuReader::builder()
    .ignore_nf_for_pp(true) // rate NF plays as though NoFail were not on them
    .build()?;
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
