# OsuMemoryReading - Development Roadmap & ToDo

This roadmap outlines the milestones required to transform `osumemoryreading` into a full-featured, zero-overhead, production-grade drop-in replacement for **tosu**.

---

## 🚀 Progress & Current Status

- [x] **Tournament Mode Real-Time Reading**:
  - [x] Ultra-fast PEB command-line extraction via `NtQueryInformationProcess(ProcessWow64Information)` (<0.05 ms).
  - [x] IPC client extraction, automatic `ipc_id` sorting (0..5), and left/right team splitting.
  - [x] Mod decryption via XOR formula `(scoreBase + 0x1c) + 0xc ^ (scoreBase + 0x1c) + 0x8` and ScoreV2 bitflag (`1 << 29`).
  - [x] Accurate grade/rank calculation (SS, S, A, B, C, D) based on accuracy, hits, and HP.
  - [x] Tournament manager state (IPC states 1, 3, 4; best of, scores, star counts, team names).
  - [x] `#multiplayer` tournament chat extraction and automatic team message attribution.
  - [x] High-performance `TournamentSession` caching: steady-state poll latency reduced from **14,000 ms -> 9.4 ms** (~1,500x speedup).
  - [x] Live end-to-end comparison vs tosu (`cargo run --release -- compare-tosu`) validated on active 3v3 match.

---

## 📌 Milestone 1: Single-Player Mode Support

Ensure single-client osu! instances (normal gameplay, practicing, solo, editor) work with the exact same speed and accuracy as tosu.

- [ ] **1.1. Status / State Engine (`status_ptr`)**:
  - [ ] Read osu! status enum (`0 = Unknown`, `1 = SelectEdit`, `2 = Play`, `3 = Edit`, `4 = ModSelect`, `5 = MatchSetup`, `7 = ResultsScreen`).
  - [ ] Map numeric status to human-readable names (`"play"`, `"menu"`, `"editor"`, `"resultScreen"`, etc.).
  - [ ] Add state change event hooks / callbacks.
- [ ] **1.2. Audio & Song Time (`play_time_addr` / `game_time_ptr`)**:
  - [ ] Read live playback time (in milliseconds) with sub-frame precision.
  - [ ] Read track audio length (`get_audio_length_ptr`) and live playback state (playing, paused).
- [ ] **1.3. Beatmap Metadata (`base_addr`)**:
  - [ ] Dereference current beatmap object:
    - [ ] `id` (Beatmap ID) & `set` (BeatmapSet ID).
    - [ ] `md5` beatmap checksum.
    - [ ] `artist`, `title`, `version` (difficulty name), `mapper`.
    - [ ] Difficulty attributes: `AR`, `CS`, `OD`, `HP`, `BPM`, `maxCombo`, `objectCount`.
    - [ ] Folder path and `.osu` file path for direct parsing if needed.
- [ ] **1.4. Local Player Profile (`user_profile_ptr`)**:
  - [ ] Read local logged-in user profile when not spectating (`name`, `id`, `accuracy`, `rank`, `pp`).
  - [ ] Seamlessly fallback between `user_profile_ptr` (single-player) and `spectating_user_ptr` (tournament/spectate).
- [ ] **1.5. Results Screen Reader (`results_screen`)**:
  - [ ] When `status == 7`, read completed performance data directly from memory:
    - [ ] Final score, accuracy, max combo, 300/100/50/0 hits, mods, and grade.
- [ ] **1.6. Unified `SinglePlayerSession`**:
  - [ ] Implement persistent address caching for single player to achieve steady-state poll latency **< 1.5 ms**.

---

## 🌐 Milestone 2: Built-in tosu-Compatible Web Server

Allow any existing tosu overlay, browser widget, or external tool to connect directly without modifying their frontend code.

- [ ] **2.1. HTTP API Server (`GET /json/v2`, `GET /json`)**:
  - [ ] Drop-in compatible JSON output mirroring tosu's exact JSON schema:
    - [ ] Single-player: `beatmap`, `state`, `play`, `profile`, `resultsScreen`.
    - [ ] Tournament: `tourney` (`ipcState`, `bestOF`, `points`, `totalScore`, `team`, `clients`, `chat`).
  - [ ] Enable CORS headers (`Access-Control-Allow-Origin: *`) for browser overlays.
  - [ ] Add health-check endpoints: `GET /health`, `GET /status`, `GET /api/v1/ping`.
- [ ] **2.2. WebSocket Server (`ws://127.0.0.1:24050/websocket/v2`)**:
  - [ ] High-frequency streaming WebSocket broadcasting JSON updates.
  - [ ] Configurable update rates (30Hz, 60Hz, 120Hz, or delta-only on memory change).
  - [ ] Low-latency broadcast channel with backpressure handling (Tokio + Tungstenite).
- [ ] **2.3. Configurable Port & Bindings**:
  - [ ] Default port `24050` with CLI overrides (e.g. `--port 24050 --host 127.0.0.1`).
  - [ ] Automatic graceful fallback if port 24050 is in use.

---

## ⚡ Milestone 3: Auto-Detection & Resilient Process Lifecycle

Make the binary / library zero-configuration and resilient to game restarts.

- [ ] **3.1. Automatic Mode Detection**:
  - [ ] Auto-detect whether the user is running Tournament mode (multiple `osu!.exe` / `-spectateclient` / `-go`) or Single-Player mode.
  - [ ] Automatically switch JSON schemas or provide unified response payloads.
- [ ] **3.2. Dynamic Attach & Hot-Reconnection**:
  - [ ] Background polling daemon that detects when `osu!.exe` starts, restarts, or exits.
  - [ ] Automatically re-scan patterns without crashing or requiring manual restarts.
  - [ ] Handle game restarts mid-match seamlessly.
- [ ] **3.3. Memory Range Adaptation**:
  - [ ] Support custom heap base ranges if osu! uses Large Address Aware (LAA) or 4GB patches.

---

## 📦 Milestone 4: Production Library Architecture (Crate)

Provide a clean, idiomatic Rust crate that other projects (like `osu_tournament_overlay`) can import directly.

- [ ] **4.1. Clean Public API**:
  - [ ] Create `OsuReader` / `OsuSession` builder:
    ```rust
    let mut reader = OsuReader::builder()
        .enable_tournament(true)
        .poll_interval(Duration::from_millis(16))
        .build()?;
    let snapshot = reader.poll()?;
    ```
  - [ ] Provide asynchronous Tokio stream:
    ```rust
    let mut stream = reader.into_stream();
    while let Some(snapshot) = stream.next().await { ... }
    ```
- [ ] **4.2. Feature Flags**:
  - [ ] `default = ["cli", "server"]`
  - [ ] `server`: pulls in Tokio, Axum/Hyper, Tungstenite for HTTP/WS serving.
  - [ ] `pp`: integrates `rosu-pp` / `rosu-mem` for live stars, PP, and FC PP calculations.
  - [ ] `minimal`: lightweight core memory reader without web servers or extra dependencies.

---

## 🐍 Milestone 5: Cross-Language Bindings

Allow overlays, bots, and analytics tools written in Python, C#, or Go to leverage the high-speed Rust memory reader.

- [ ] **5.1. Python Bindings (`pyo3` / `maturin`)**:
  - [ ] Expose native Python module `osumemory`:
    ```python
    import osumemory

    session = osumemory.TournamentSession()
    data = session.poll()
    print(data.clients[0].gameplay.score)
    ```
  - [ ] Pre-compiled wheels for Windows x64.
- [ ] **5.2. C-ABI Shared Library (`osumemory.dll`)**:
  - [ ] Provide C headers (`osumemory.h`) with stable FFI functions:
    - `osu_session_create()`, `osu_session_poll_json()`, `osu_session_free()`.
  - [ ] C# P/Invoke wrapper for WPF/Avalonia/.NET tournament overlays.

---

## 📊 Milestone 6: Performance, Calculations & Polish

- [ ] **6.1. Live PP Calculation Integration**:
  - [ ] Embed `rosu-pp` to calculate live `current_pp` and `fc_pp` on-the-fly for every spectator and solo player.
- [ ] **6.2. Zero-Copy Serialization**:
  - [ ] Optimize JSON serialization to directly stream into WebSocket frame buffers without intermediate string allocations.
- [ ] **6.3. Comprehensive Test Suite & Mocks**:
  - [ ] Create simulated memory dumps for CI testing without needing live `osu!.exe` running.
