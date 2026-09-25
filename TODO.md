# rtosu-dataprovider - Development Roadmap & ToDo

This roadmap outlines the milestones required to transform `rtosu-dataprovider` into a full-featured, zero-overhead, production-grade drop-in replacement for **tosu**.

---

## 🚀 Progress & Current Status

- [x] **Tournament Mode Real-Time Reading**:
  - [x] Ultra-fast PEB command-line extraction via `NtQueryInformationProcess(ProcessWow64Information)` (<0.05 ms).
  - [x] IPC client extraction, automatic `ipc_id` sorting (0..5), and left/right team splitting.
  - [x] Mod decryption via XOR formula `(scoreBase + 0x1c) + 0xc ^ (scoreBase + 0x1c) + 0x8` and ScoreV2 bitflag (`1 << 29`).
  - [x] Complete 31-bit mod flag table with mutual exclusivity (NC overrides DT, PF overrides SD, CN overrides AT) and tosu `play.mods.array` support.
  - [x] Accurate grade/rank calculation (SS, S, A, B, C, D) based on accuracy, hits, and HP.
  - [x] Tournament manager state (IPC states 1, 3, 4; best of, scores, star counts, team names).
  - [x] `#multiplayer` tournament chat extraction and automatic team message attribution.
  - [x] High-performance `TournamentSession` caching: steady-state poll latency reduced from **14,000 ms -> 9.4 ms** (~1,500x speedup).
  - [x] Live end-to-end comparison vs tosu (`cargo run --release -- compare-tosu`) validated on active 3v3 match.
- [x] **Configuration System (`config.toml`)**:
  - [x] TOML-based configuration file (`config.toml`) with serde serialization/deserialization.
  - [x] Exhaustive in-file comments explaining every variable, default values, and valid min/max ranges.
  - [x] CLI commands: `config show`, `config init`, and `config validate`.
  - [x] Global `--config <path>` flag support.

---

## 📌 Milestone 1: Single-Player Mode Support

Ensure single-client osu! instances (normal gameplay, practicing, solo, editor) work with the exact same speed and accuracy as tosu.

- [x] **1.1. Status / State Engine (`status_ptr`)**:
  - [x] Read osu! status enum (`0 = Unknown`, `1 = SelectEdit`, `2 = Play`, `3 = Edit`, `4 = ModSelect`, `5 = MatchSetup`, `7 = ResultsScreen`).
  - [x] Map numeric status to human-readable names (`"play"`, `"menu"`, `"editor"`, `"resultScreen"`, etc.).
  - [x] Dynamic foreground window focus checking (`game.focused`).
- [x] **1.2. Audio & Song Time (`play_time_addr` / `game_time_ptr`)**:
  - [x] Read live playback time (in milliseconds) with sub-frame precision.
  - [x] Read track audio length (`get_audio_length_ptr`) and live playback state (playing, paused).
- [x] **1.3. Beatmap Metadata (`base_addr`)**:
  - [x] Dereference current beatmap object:
    - [x] `id` (Beatmap ID) & `set` (BeatmapSet ID).
    - [x] `md5` beatmap checksum.
    - [x] `artist`, `title`, `version` (difficulty name), `mapper`.
    - [x] Difficulty attributes: `AR`, `CS`, `OD`, `HP`, `BPM` (min, max, common, realtime), `maxCombo`, `objectCount`.
    - [x] Accurate slider end duration calculation (`start_time + dist / velocity`).
    - [x] Folder path and `.osu` file path resolution via process directory.
- [x] **1.4. Local Player Profile (`user_profile_ptr`)**:
  - [x] Read local logged-in user profile when not spectating (`name`, `id`, `accuracy`, `rank`, `pp`).
  - [x] Seamless fallback between `user_profile_ptr` (single-player) and `spectating_user_ptr` (tournament/spectate).
- [x] **1.5. Results Screen Reader (`results_screen`)**:
  - [x] When `status == 7`, read completed performance data directly from memory:
    - [x] Final score, accuracy, max combo, 300/100/50/0 hits, mods, PP, and grade.
- [x] **1.6. Unified `SoloSession`**:
  - [x] Implement persistent address and metadata caching for single player to achieve steady-state poll latency **< 0.5 ms** and CPU **< 0.2%**.

---

## 🌐 Milestone 2: Built-in tosu-Compatible Web Server

Allow any existing tosu overlay, browser widget, or external tool to connect directly without modifying their frontend code.

- [x] **2.1. HTTP API Server (`GET /json/v2`, `GET /json`)**:
  - [x] Drop-in compatible JSON output mirroring tosu's exact JSON schema:
    - [x] Single-player: `beatmap`, `state`, `play`, `profile`, `resultsScreen`.
    - [x] Tournament: `tourney` (`ipcState`, `bestOF`, `points`, `totalScore`, `team`, `clients`, `chat`).
  - [x] Enable CORS headers (`Access-Control-Allow-Origin: *`) for browser overlays.
  - [x] Add health-check endpoint: `GET /health`.
- [x] **2.2. WebSocket Server (`ws://127.0.0.1:24050/websocket/v2`)**:
  - [x] High-frequency streaming WebSocket broadcasting JSON updates at 60 Hz.
  - [x] Low-latency broadcast channel with backpressure handling (Tokio + Tungstenite + Axum).
- [x] **2.3. Configurable Port, Bindings & Zero-Port Bypass**:
  - [x] Default port `24050` with CLI overrides (e.g. `--port 24050 --host 127.0.0.1`).
  - [x] Port configurable in `config.toml` and CLI.
  - [x] Conditional route mounting for `enable_http` and `enable_websocket`.
  - [x] Zero-port bypass: when both are disabled, server skips binding any TCP socket or listener.
- [x] **2.4. Production Structured Logging & Log Retention**:
  - [x] Tracing structured logging system respecting `[logging.level]`.
  - [x] Daily rolling log file writer emitting `logs/rtosu-YYYY-MM-DD.log`.
  - [x] Automatic log retention pruner keeping up to `max_log_files` (default: 7) days of logs.

---

## ⚡ Milestone 3: Auto-Detection & Resilient Process Lifecycle

Make the binary / library zero-configuration and resilient to game restarts.

- [x] **3.1. Automatic Mode Detection**:
  - [x] Auto-detect whether the user is running Tournament mode (multiple `osu!.exe` / `-spectateclient` / `-go`) or Single-Player mode.
  - [x] Seamlessly transition JSON schemas between solo and tournament feeds.
- [x] **3.2. Dynamic Attach & Hot-Reconnection**:
  - [x] Zero-overhead process checking using `GetExitCodeProcess` (~50ns).
  - [x] Throttled process re-enumeration (1.5s interval) when detached or restarted.
  - [x] Automatically re-scan patterns without crashing or requiring manual restarts.
  - [x] Handle game restarts mid-match seamlessly.
- [x] **3.3. Memory Range Adaptation**:
  - [x] Support pointer widths (32-bit and 64-bit) and configurable scan limit bytes.

---

## 📦 Milestone 4: Production Library Architecture (Crate)

Provide a clean, idiomatic Rust crate that other projects (like `osu_tournament_overlay`) can import directly.

- [x] **4.1. Clean Public API**:
  - [x] Public crate structure (`session::SoloSession`, `session::TournamentSession`, `server::start_server`, `v2::TosuV2Packet`).
  - [x] High-level `OsuReaderBuilder` and `OsuReader` convenience wrapper:
    ```rust
    let mut reader = OsuReader::builder()
        .poll_interval(Duration::from_millis(16))
        .build()?;
    let packet = reader.poll()?;
    ```
  - [x] Asynchronous Tokio stream helper (`reader.into_stream()`).
  - [x] In-process direct memory reading without running the HTTP or WebSocket server by default.
- [x] **4.2. Feature Flags**:
  - [x] `default = ["pp"]`: PP calculation enabled by default with zero extra flags needed.
  - [x] `rosu-mem` / `rosu-memory`: optional integration with external memory scanner crates.

---

## 🐍 Milestone 5: Cross-Language Bindings

Allow overlays, bots, and analytics tools written in Python, C#, or Go to leverage the high-speed Rust memory reader.

- [ ] **5.1. Python Bindings (`pyo3` / `maturin`)**:
  - [ ] Expose native Python module `rtosu`:
    ```python
    import rtosu

    session = rtosu.TournamentSession()
    data = session.poll()
    print(data.clients[0].gameplay.score)
    ```
  - [ ] Pre-compiled wheels for Windows x64.
- [ ] **5.2. C-ABI Shared Library (`rtosu.dll`)**:
  - [ ] Provide C headers (`rtosu.h`) with stable FFI functions:
    - `rtosu_session_create()`, `rtosu_session_poll_json()`, `rtosu_session_free()`.
  - [ ] C# P/Invoke wrapper for WPF/Avalonia/.NET tournament overlays.

---

## 📊 Milestone 6: Performance, Calculations & Polish

- [x] **6.1. Live PP Calculation Integration**:
  - [x] Embed `rosu-pp` with modern CSR rework support.
  - [x] 10-object stepping gradual PP progression calculation.
  - [x] Precalculated accuracy curve table (90%..100%).
  - [x] Strain graphs (aim, speed, reading, flashlight).
- [x] **6.2. High-Performance Steady-State Engine**:
  - [x] Cached difficulty attributes and beatmap metadata by `(checksum, mods)`.
  - [x] Gated PP recalculations by hit state changes.
  - [x] Memory usage reduced to **~6–12 MB RAM**, CPU to **~0.1%–0.2%**.
- [x] **6.3. Comprehensive Test Suite**:
  - [x] 22 passing unit tests covering serialization, addresses, mods, patterns, profiles, tournaments, and PP calculations.
