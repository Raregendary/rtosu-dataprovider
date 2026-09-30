# Changelog

All notable changes to `rtosu-dataprovider` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Two notes on how to read this file:

* Entries are written for someone upgrading a tournament setup, not for someone reading the
  diff. Where a change is only observable on the wire, the payload key is named.
* Everything up to and including `1.0.7` was backfilled from the git history and tag list
  when this file was created, so those sections are release-level summaries rather than a
  commit-by-commit log. From `1.0.8` onward, each release section is written as the work
  lands (see `1.0.8-updateplan.md` §1.3).

## [Unreleased]


## [1.0.8] - 2026-09-30

The 1.0.8 line.

### Added

* **Mod score multipliers** (`[scoring]`): per-mod score weights, so a tournament can rate a
  play by its mods. While enabled, `play.score`, `resultsScreen.score`, and each
  `tourney.clients[].play.score` are multiplied by the factors of the mods the play carries
  (`{ "EZ" = 1.8, "NF" = 0.5 }`, and factors of different mods multiply), and
  `tourney.totalScore.left/right` becomes the sum of the weighted client scores — the
  tournament manager's own total is a sum of *unweighted* scores and cannot be rescaled once
  any client carries a different weight. Off by default, and the shipped table lists **every**
  mod at `1.0`, so the file itself shows which keys exist and nothing moves until one is
  edited. A key may name an osu! slot as a group (`"DT/NC"`, because Nightcore sets the
  DoubleTime bit as well, as do `"SD/PF"` and `"AT/CN"`), which is counted once rather than
  twice. An unknown acronym is rejected by `config validate` instead of being ignored.
  Accuracy, rank, pp and the leaderboard are never weighted.
* **Settings landing page** at `http://127.0.0.1:24050/`: view and edit the configuration
  from a browser. Every `config.toml` setting renders as a form control inside a
  collapsible section (open/closed states are remembered between loads), above the
  overlay cards — shown full width, the same cards the `/overlays` dashboard renders —
  an index of every endpoint, and a footer linking the repository and crediting the
  original [tosu](https://tosu.app/) project whose API this server reproduces. Backed by
  `GET /api/settings` and `POST /api/settings` (a flat patch of dot-path keys, validated by
  the same code that validates the config file at startup). Edits are written back to
  `config.toml` with every comment preserved, and the ones that can take effect without a
  restart (`[features]`, `[scoring]`, `poll.poll_rate_hz`, `settings_write_local_only`)
  are applied live.
* **Hot restart** (`POST /api/restart`, and a **↻ Restart** button in the page header):
  the running process stops listening, starts a fresh copy of itself from the same command
  line, and exits — so settings that are only read at startup take effect without touching
  a terminal, and the page reloads itself once the new process answers. Guarded more
  strictly than settings writes and not configurable: loopback peer with a loopback (or
  absent) `Origin` only, and only where a process supervisor is attached — a library
  consumer that never wired one gets `409` instead of a silently ignored request.
* `server.settings_write_local_only` (default `true`): when the server is bound to `0.0.0.0`,
  only loopback clients may change settings. Everyone on the LAN can still view the page and
  the settings. This is a convenience guard, not authentication.
* `features.ignore_nf_for_pp` (default `false`): compute PP as if the NoFail mod were not
  present, for tournaments that force NF on every player. `play.mods` still reports NF and
  star rating, accuracy, hits, combo and rank are untouched — only the PP family moves
  (`play.pp`, `resultsScreen.pp`, `beatmap.stats.pp`, and the pp derived for tournament
  clients), so a lobby that is forced onto NF is rated on the play's own merit. What the
  toggle gives back depends on the ruleset and the scoreline, because that is what osu! takes
  away: osu!mania pays a flat ~33 % more (the calculator's ×0.75), osu!standard and osu!catch
  pay `(1 - 0.02 × misses).max(0.9)`, so a missless FC is unchanged and a play with five or
  more misses gains ~11 %, and osu!taiko is a no-op because our calculator applies no NF
  penalty there.
* This changelog.

### Changed

* `reader::format_tourney_packet` takes the score-multiplier table as a third
  argument (`&scoring::ModMultipliers`). Library consumers that call it directly
  can pass `&ModMultipliers::identity()` for the previous behaviour, or
  `scoring::ModMultipliers::new(&table)?` to weight; `OsuReaderBuilder`'s
  `mod_multipliers(...)` is the same setting for the builder path.
* `server::start_server` and `server::serve_with_listener` take the settings
  store as an `Option<Arc<settings::SettingsStore>>` argument followed by an
  `Option<server::RestartSignal>` (from `RestartSignal::channel()`, wired by the
  CLI so `POST /api/restart` has a supervisor to raise; `None` answers `409`),
  and the listener is now served with connect info
  (`into_make_service_with_connect_info`) so the settings write guard can tell a
  local request from a remote one. A library that builds the router itself with
  `create_router_with` should serve it the same way; without connect info a write
  is refused rather than assumed local.
* `AppConfig::save_preserving_comments(path, previous)` is the writer behind the
  page's save button, and is public for consumers that keep their own
  `config.toml`: it patches the file's text, so comments, key order and every
  unedited line survive, and a key or section the file is missing is appended in
  place.
* The poll loop reads its settings from `settings::LiveSettings` (`settings::SettingsStore`)
  instead of from the config captured at startup, which is what makes a change on
  the landing page take effect on the next tick. `OsuReader::apply_live_settings`
  and the per-setting setters next to it are the library-level form of the same
  thing.
* The shipped config template documents `server.json_payload`, which it had been
  missing since that option was added, so a fresh install's file no longer gains
  the key on its first settings save. `README.md` no longer lists the removed
  `features.gradual_pp_chunks`.
* Development notes, release specs, and audit records no longer sit in the repository root.
  They live in the ignored `validations/notes/`, `validations/audits/`, and
  `validations/scratch/` directories, and `.gitignore` matches them by pattern.

### Known divergences

* `POST /api/settingsSave` is deliberately not implemented: tosu's path takes an Electron
  dashboard configuration record with a different schema, and serving the same path with a
  different body would be worse than not having it. rtosu's equivalent is
  `POST /api/settings`.

## [1.0.7] - 2026-09-29

Performance and memory release. Measured against a live osu!: 3-6 threads (from 16-32),
3.4-3.9 MB private commit, 0.03 % CPU, with the v1 `strainsAll` series, key order, and point
count unchanged from tosu's.

### Changed

* Dropped the `rayon` feature from `rosu-pp-gemini`. The pool is sized to the logical core
  count and exists to parallelise work rtosu does one map at a time, at the cost of a
  permanent working-set jump from the thread stacks and TLS blocks.
* The Tokio runtime is built with `worker_threads(2)` instead of one worker per logical
  core. The poll is a blocking call on its own thread, so the extra workers only cost a
  stack each.
* Tournament live PP is recomputed only when its inputs change (mod bits, the five judgement
  counts, the hit total) rather than running two `rosu_pp::Performance` evaluations per
  client per tick. The evaluation is invalidated on a beatmap checksum change.
* `v1.strainsAll` and `v2.strains` no longer deep-copy the decoded strain graph on every
  request and every frame (three copies of a ~250 KB structure). `V1StrainsAll` now borrows
  the shared `Arc<PerformanceGraph>`, with `Serialize` delegating to it so the wire shape and
  key order cannot drift.

### Fixed

* Leaderboard reads are rate-limited to 1 Hz instead of once per tick. Walking 50-100
  scoreboard entries with ~18 `ReadProcessMemory` calls each was spending tens of thousands
  of kernel transitions per second on unchanged data. The timer is backdated at every
  play-state reset, so a new map still reads its list on the first tick.

### Notes

* Three of the eight tasks in the 1.0.7 plan were not implemented, each with a reason
  recorded in the release commit: the `tx.receiver_count() == 0` serialization gate (the
  router holds a receiver for the life of the process, so it would be dead code), replacing
  `Utf8Bytes::try_from` with an unchecked conversion (the method does not exist on the
  tungstenite version axum 0.8 pulls in, and it would trade a tested guard for skipping a
  scan), and the `/tokens` `filters.is_empty()` short-circuit (already present).

## [1.0.6] - 2026-09-28

tosu parity release: the two payload values a live `/json/v2` diff against tosu found wrong.

### Added

* `play.hits.sliderBreaks` is inferred the way tosu infers it — a combo drop that is not
  accompanied by a miss — instead of being hardcoded to `0`.
* `beatmap.stats.stars.live` now reports the difficulty of the objects judged so far
  (`GradualCursor`), stepping the real difficulty curve as judgements arrive. It replaces the
  precomputed chunk vector, which cost as much on an untouched map as on a finished one and
  forced a worker thread on large maps.

### Changed

* Removed the obsolete `features.gradual_pp_chunks` option; the incremental cursor has no
  chunk count to tune.
* `rosu-pp-gemini` was built with its `rayon` feature for this release to halve the one-shot
  attach pass. Reverted in 1.0.7, which found the thread pool cost more than it saved.

## [1.0.5] - 2026-09-28

tosu endpoint and payload parity, plus the poll rate cap. Every route tosu registers now
exists here under the same path, with the same body.

### Added

* `GET /json` serves the gosumemory-compatible **v1** payload, which is what tosu serves on
  that path. Set `server.json_payload = "v2"` to put rtosu's older v2 payload back on it;
  `/json/v2` is unaffected either way. `GET /json/v1` is an explicit alias.
* `GET /json/sc`, the StreamCompanion payload (136 flat keys), and `WS /tokens`, its socket,
  with the `applyFilters` message handling.
* `WS /websocket/commands`, tosu's inbound-only command channel.
* `GET /files/beatmap/{*path}`, `GET /Songs/{*path}`, `GET /files/skin/{*path}`, and
  `GET /backgroundImage`, so drop-in overlays that load audio, images, or `.osu` files by
  path work unmodified.

### Changed

* `poll.poll_rate_hz` is capped at 120 Hz instead of 1000. osu! stable cannot update faster
  than that, so above it every poll re-reads unchanged memory and spends a proportional share
  of a core doing it.

### Fixed

* Four payload values corrected against a live tosu: `game.paused`, the leaderboard, the
  `files.background` filename, and v1's `menu.bm.time.current`.
* A beatmap switch no longer latches the previous map's data until the next read.

## [1.0.4] - 2026-09-27

Hardening release: the panic-site audit and the provider bugs it turned up.

### Changed

* The crate builds without the default `pp` feature.

### Fixed

* Every panic-capable site removed from the live code paths. The release profile uses
  `panic = "abort"`, so one `unwrap` on a bad read would have killed the server mid-match for
  every connected client at once.
* The beatmap object type is parsed before the map's time range is widened, so a type byte
  is not read as a timestamp.
* Clock-rate BPM conversion made idempotent; the taiko star and hit-window breakdown is
  populated instead of left blank.
* Tournament flags are matched as whole arguments. A file path containing the word
  "tournament" no longer classifies a solo osu! as a tournament manager.
* osu!catch and osu!mania result-screen accuracy corrected.
* Tournament chat changes are detected by the newest message rather than a count, so a
  replaced message is not missed.
* A tournament map change that reuses the same beatmap object is detected.
* A finished play is dropped when osu! leaves the map, instead of being served as live.
* Beatmap backgrounds are contained inside the songs folder, and overlay slugs are
  percent-encoded, so a crafted background path cannot escape the songs directory.

## [1.0.3] - 2026-09-27

Browser overlay hosting, plus the provider corrections that building the example overlays
surfaced.

### Added

* Drop-in **tosu v2 compatible browser overlays**: put a folder with an `index.html` into
  `browser_overlays/` and use it as an OBS Browser source. A generated compatibility shim
  rewrites the overlay's tosu API calls to this server, so unmodified overlays work.
* `GET /overlays`, a dashboard listing every discovered overlay with its copy-ready URL and
  metadata (`metadata.txt`).
* Two example overlays: a solo provider readout and a tournament scoreboard with team
  colours, per-team ranking, and a set-score bar.

### Fixed

* Solo play state no longer survives a map change.
* `pp.fc` is computed outside gameplay, so it no longer appears only once a play starts.
* Tournament clients no longer all report a fabricated `XH` rank; an unreadable gameplay
  state reports an empty rank instead.
* Beatmap metadata no longer goes blank for the rest of a map when the first read lands
  before osu! has finished assembling it.
* Overlay rendering: romanized titles, difficulty and mapper, and right-anchored columns.

## [1.0.2] - 2026-09-26

Performance release and the first full tosu v2 parity pass.

### Added

* Criterion benchmarks (`benches/micro.rs`, `benches/live_harness.rs`) and the `instr`
  per-phase timing counters.

### Changed

* Tournament spectator polling loop reworked: CPU usage below tosu's on the same match.
* Mod formatting cached per mod bitmask; the hit-error array read into a reusable scratch
  buffer.
* Strain graph JSON serialization overhead removed, read buffers moved onto the stack, and
  solo process-discovery spikes eliminated.

### Fixed

* Full tosu v2 parity across solo and tournament modes, including tournament hit-error
  payloads, the menu mod dereference, live gradual PP in tournament clients, beatmap status
  propagation, and country casing.

## [1.0.1] - 2026-09-25

### Changed

* `rosu-pp` is taken from crates.io (`rosu-pp-gemini 5.0.3`) instead of a local path, so the
  crate is reproducible from a clean checkout.
* Author email corrected.

## [1.0.0] - 2026-09-25

Initial release: a native Rust, tosu-compatible data provider for osu!.

### Added

* Memory reader for osu! stable with automatic solo/tournament detection, IPC client
  discovery, left/right team splitting, tournament manager state, and `#multiplayer` chat
  extraction.
* tosu-compatible server: `GET /json/v2`, `WS /websocket/v2`, `GET /health`, and the file
  routes overlays use to load beatmap backgrounds.
* Gradual live PP calculation (`rosu-pp` with combo scaling removal) and grade/rank
  calculation.
* `config.toml` with in-file documentation of every setting, plus `config show`, `config
  init`, and `config validate`.
* Windows release packaging and CI (`.github/workflows/{ci,release}.yml`), tag-triggered.

[Unreleased]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.8...HEAD
[1.0.8]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.7...v1.0.8
[1.0.7]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.6...v1.0.7
[1.0.6]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/Raregendary/rtosu-dataprovider/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/Raregendary/rtosu-dataprovider/releases/tag/v1.0.0

