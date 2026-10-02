use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub const DEFAULT_CONFIG_FILE: &str = "config.toml";

/// The highest `poll.poll_rate_hz` a config may ask for.
///
/// 120 Hz, not a round number by accident: it is the fastest osu! stable
/// itself updates, so anything above it re-reads the same memory for every real
/// change. See [`PollConfig::poll_rate_hz`].
pub const MAX_POLL_RATE_HZ: u32 = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub poll: PollConfig,
    pub features: FeatureConfig,
    pub scoring: ScoringConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Network interface address to bind to (e.g. "127.0.0.1" for localhost, "0.0.0.0" for LAN)
    pub host: String,
    /// Listening TCP port for tosu HTTP/WebSocket drop-in replacement (default: 24050, min: 1024, max: 65535)
    pub port: u16,
    /// Enable CORS headers (Access-Control-Allow-Origin: *) for browser-based overlays
    pub cors_allow_all: bool,
    /// Enable WebSocket broadcasting stream on /websocket/v2
    pub enable_websocket: bool,
    /// Enable HTTP REST endpoints on /json, /json/v1, /json/v2, /json/v2/precise, /json/sc and /health
    pub enable_http: bool,
    /// Which payload `GET /json` serves.
    ///
    /// **This was a breaking change and this is the escape hatch.** tosu serves
    /// the gosumemory-compatible v1 payload at `/json`
    /// (`packages/server/router/index.ts:43-53`) and the v2 payload at
    /// `/json/v2`, so rtosu matches it -- which means an existing rtosu consumer
    /// reading v2 from `/json` now receives a different shape on the same URL,
    /// with no error signal. Set this to `"v2"` to put `/json` back where it was
    /// before; `/json/v2` serves the v2 payload either way, so nothing else
    /// changes.
    ///
    /// Accepted values are `"v1"` (the default, matching tosu) and `"v2"`. Anything
    /// else logs a warning and falls back to `"v1"`, because a typo must not
    /// silently serve the shape the operator was trying to avoid.
    pub json_payload: String,
    /// Serve user-supplied, tosu v2 API compatible browser overlays from a
    /// directory of overlay folders and render a dashboard to browse them
    pub enable_overlays: bool,
    /// Directory containing one subfolder per browser overlay, each with an
    /// index.html. Relative paths resolve against the working directory.
    pub overlays_dir: String,
    /// Accept settings writes (`POST /api/settings`) only from the loopback
    /// interface. See `src/settings.rs`.
    ///
    /// Readers on the LAN can still view the landing page (`GET /`) and read
    /// `GET /api/settings`; they just cannot change anything. Moot when
    /// `host = "127.0.0.1"` (the default), which is the real boundary -- this
    /// exists so `host = "0.0.0.0"` does not hand the configuration to every
    /// machine on the venue network.
    ///
    /// **A convenience guard, not authentication.** Anything that can open a
    /// socket from the host machine is still inside it, which is why the
    /// landing page must never be exposed beyond a trusted network.
    pub settings_write_local_only: bool,
    /// WebSocket per-socket initial write buffer size in bytes (default: 64 KB).
    pub ws_write_buffer_size: usize,
    /// WebSocket per-socket max write buffer size in bytes (default: 512 KB).
    /// Bounds backpressure memory for slow or congested clients.
    pub ws_max_write_buffer_size: usize,
    /// WebSocket per-socket max frame size in bytes (default: 16 MB).
    pub ws_max_frame_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PollConfig {
    /// Polling frequency in Hertz / frames per second (default: 60 Hz, min: 1 Hz, max: 120 Hz)
    ///
    /// 120 Hz is the ceiling because it is the fastest the game itself updates:
    /// osu! stable renders on the display's refresh, and no consumer of this
    /// payload can observe a change that the game has not made yet. Above 120 Hz
    /// the poll reads the same memory twice for every real update, so the extra
    /// frequency buys no fresher data and costs a proportional share of a core
    /// in `ReadProcessMemory` syscalls.
    ///
    /// 60 Hz = ~16.6 ms interval; 120 Hz = ~8.3 ms interval
    pub poll_rate_hz: u32,
    /// Memory signature scanning budget in Megabytes (default: 128 MB, min: 16 MB, max: 1024 MB)
    pub scan_budget_mb: usize,
    /// Memory pattern profile to load (default: "tournament", options: "tournament", "stable")
    pub default_profile: String,
    /// Automatically switch between Tournament mode and Single-Player mode based on running osu! processes
    pub auto_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureConfig {
    /// Read and attribution of multiplayer tournament chat messages from memory
    pub enable_chat: bool,
    /// Optional real-time PP calculation (requires feature 'rosu-mem' or 'pp')
    pub enable_pp: bool,
    /// Compute PP as if the NoFail mod were not on the play.
    ///
    /// For tournaments that force NF on everybody and want the rating the play
    /// would have been worth without it. `play.mods` still reports NF, and star
    /// rating, accuracy, hits and rank do not move -- only the pp family.
    /// No-op on osu!taiko, whose calculator applies no NF penalty.
    pub ignore_nf_for_pp: bool,
    /// Number of gradual PP chunks per beatmap (1 = full map only / no gradual, max = 250, default = 100)
    /// Include hit error array in JSON packet (if false, sends [] while still calculating unstableRate)
    pub enable_hit_errors: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScoringConfig {
    /// Weight the reported score by the mods the play was set on.
    ///
    /// Off by default: a data provider that silently reports numbers the game
    /// did not is worse than one that does not have the feature. While off, the
    /// table below is parsed and validated but never applied, which is what
    /// makes it safe to edit before turning this on.
    pub enable_mod_multipliers: bool,
    /// Mod acronym -> score factor, multiplied together across the mods a play
    /// has (see [`crate::scoring`]).
    ///
    /// Keys are the acronyms `play.mods.name` reports, plus `"NM"` for a
    /// modless play, and a key may name one osu! slot as a group (`"DT/NC"`,
    /// because Nightcore sets the DoubleTime bit too). The default lists every
    /// slot at 1.0: the in-game score, unchanged, but visible.
    pub mod_multipliers: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Logging verbosity level ("trace", "debug", "info", "warn", "error")
    pub level: String,
    /// Output logs to daily files in the logs/ directory
    pub log_to_file: bool,
    /// Maximum number of daily log files to retain before pruning oldest (default: 7, range: 1..=365)
    pub max_log_files: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            poll: PollConfig::default(),
            features: FeatureConfig::default(),
            scoring: ScoringConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 24050,
            cors_allow_all: true,
            enable_websocket: true,
            enable_http: true,
            json_payload: "v1".to_string(),
            enable_overlays: true,
            overlays_dir: "browser_overlays".to_string(),
            // Local writes only. The default host is loopback anyway, so this
            // only has an effect once the operator publishes the port.
            settings_write_local_only: true,
            ws_write_buffer_size: 64 * 1024,
            ws_max_write_buffer_size: 512 * 1024,
            ws_max_frame_size: 16 * 1024 * 1024,
        }
    }
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            poll_rate_hz: 60,
            scan_budget_mb: 128,
            default_profile: "tournament".to_string(),
            auto_mode: true,
        }
    }
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            enable_chat: true,
            enable_pp: true,
            ignore_nf_for_pp: false,
            enable_hit_errors: true,
        }
    }
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            enable_mod_multipliers: false,
            // Every mod the game can report, at 1.0. See `scoring.rs`: the table
            // is the documentation of which keys exist.
            mod_multipliers: crate::scoring::default_multipliers(),
        }
    }
}

impl ScoringConfig {
    /// The multipliers the reader should use: the configured table when the
    /// feature is on, and an identity table when it is off.
    ///
    /// This is where `enable_mod_multipliers` is applied, so the reader never
    /// has to carry both the flag and the table and ask about them separately.
    /// The table was validated by [`AppConfig::validate`] before anything could
    /// reach here, so a parse failure at this point is a programming error and
    /// is treated as one.
    pub fn resolved_multipliers(&self) -> Result<crate::scoring::ModMultipliers> {
        if !self.enable_mod_multipliers {
            return Ok(crate::scoring::ModMultipliers::identity());
        }
        crate::scoring::ModMultipliers::new(&self.mod_multipliers).with_context(|| {
            "scoring.mod_multipliers is invalid; run `config validate` for the exact key"
                .to_string()
        })
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            log_to_file: true,
            max_log_files: 7,
        }
    }
}

impl AppConfig {
    /// Load config from file if exists, or create default config file on disk and return it
    pub fn load_or_init<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            Self::load_from_file(path)
        } else {
            let config = Self::default();
            config.save_default_template(path)?;
            Ok(config)
        }
    }

    /// Load config from an existing file
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("reading config file at {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("parsing TOML config file at {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate values are within safe min/max ranges
    pub fn validate(&self) -> Result<()> {
        if self.server.port < 1024 {
            anyhow::bail!("server.port must be >= 1024 (got {})", self.server.port);
        }
        if self.server.enable_overlays {
            if !self.server.enable_http {
                anyhow::bail!(
                    "server.enable_overlays requires server.enable_http = true; overlay pages and assets are served over HTTP"
                );
            }
            if self.server.overlays_dir.trim().is_empty() {
                anyhow::bail!("server.overlays_dir must not be empty when overlays are enabled");
            }
        }
        if self.server.ws_write_buffer_size < 1024
            || self.server.ws_write_buffer_size > 16 * 1024 * 1024
        {
            anyhow::bail!(
                "server.ws_write_buffer_size must be between 1024 and 16777216 bytes (got {})",
                self.server.ws_write_buffer_size
            );
        }
        if self.server.ws_max_write_buffer_size < self.server.ws_write_buffer_size
            || self.server.ws_max_write_buffer_size > 64 * 1024 * 1024
        {
            anyhow::bail!(
                "server.ws_max_write_buffer_size must be >= ws_write_buffer_size and <= 67108864 bytes (got {})",
                self.server.ws_max_write_buffer_size
            );
        }
        if self.server.ws_max_frame_size < 1024 || self.server.ws_max_frame_size > 64 * 1024 * 1024
        {
            anyhow::bail!(
                "server.ws_max_frame_size must be between 1024 and 67108864 bytes (got {})",
                self.server.ws_max_frame_size
            );
        }
        if self.poll.poll_rate_hz == 0 || self.poll.poll_rate_hz > MAX_POLL_RATE_HZ {
            anyhow::bail!(
                "poll.poll_rate_hz must be between 1 and {MAX_POLL_RATE_HZ} Hz (got {})",
                self.poll.poll_rate_hz
            );
        }
        if self.poll.scan_budget_mb < 16 || self.poll.scan_budget_mb > 2048 {
            anyhow::bail!(
                "poll.scan_budget_mb must be between 16 and 2048 MB (got {})",
                self.poll.scan_budget_mb
            );
        }
        if self.logging.max_log_files == 0 || self.logging.max_log_files > 365 {
            anyhow::bail!(
                "logging.max_log_files must be between 1 and 365 (got {})",
                self.logging.max_log_files
            );
        }
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.logging.level.to_ascii_lowercase().as_str()) {
            anyhow::bail!(
                "logging.level must be one of: trace, debug, info, warn, error (got '{}')",
                self.logging.level
            );
        }
        // The table is validated whether or not the feature is on: a config that
        // only fails once the switch is flipped is a trap, and the whole point
        // of shipping every mod at 1.0 is that an operator edits the table first
        // and enables it afterwards.
        let multipliers = crate::scoring::ModMultipliers::new(&self.scoring.mod_multipliers)?;
        if self.scoring.enable_mod_multipliers && multipliers.is_empty() {
            tracing::warn!(
                "scoring.enable_mod_multipliers is on but scoring.mod_multipliers is empty; every score is reported unchanged. Add an entry such as \"EZ\" = 1.8, or delete the table to silence this."
            );
        }
        Ok(())
    }

    /// Calculate poll interval duration in milliseconds from configured poll_rate_hz
    pub fn poll_interval_ms(&self) -> u64 {
        if self.poll.poll_rate_hz == 0 {
            16
        } else {
            (1000 / self.poll.poll_rate_hz as u64).max(1)
        }
    }

    /// Save a documented template config.toml with explanatory comments and constraints
    pub fn save_default_template<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let template = Self::generate_documented_template();
        fs::write(path.as_ref(), template)
            .with_context(|| format!("writing default config to {}", path.as_ref().display()))?;
        Ok(())
    }

    /// Write `self` to `path`, changing only the value tokens of the leaves that
    /// differ from `previous`.
    ///
    /// [`Self::save_default_template`] writes the documented template verbatim,
    /// and every comment in it is the reason a setting exists. A
    /// `toml::to_string_pretty` round-trip would delete all of them, so this
    /// patches the text instead: a line whose key is unchanged is copied byte for
    /// byte, and only a changed leaf's value token is replaced. Nothing else is
    /// touched -- not the comments, not the key order, not the quoting style, and
    /// not any line the patch does not mention.
    ///
    /// * The base text is `previous` when it parses, otherwise the file on disk,
    ///   otherwise the documented template -- so a missing or hand-broken file
    ///   still ends up with a complete, documented config rather than a
    ///   comment-free serialisation.
    /// * A key the base does not carry is appended inside its own section, and a
    ///   section the base does not carry is appended with its header. That is how
    ///   a config written before `[scoring]` existed gains it on the first save.
    /// * Scalars are formatted the way the template writes them: bare integers,
    ///   floats with a decimal point, quoted strings, and an inline table with
    ///   quoted keys for `mod_multipliers`.
    /// * The write goes through a sibling temporary file and a rename, so an
    ///   interrupted save cannot leave a truncated config behind.
    pub fn save_preserving_comments<P: AsRef<Path>>(
        &self,
        path: P,
        previous: Option<&str>,
    ) -> Result<()> {
        /// One leaf the writer knows about, in the order the field table lists
        /// it -- which is also the order an appended key lands in.
        struct WantedField {
            key: &'static str,
            section: &'static str,
            leaf: &'static str,
            value: String,
        }

        let path = path.as_ref();
        let (base_text, base) = Self::patch_base(path, previous)?;

        let wanted: Vec<WantedField> = crate::settings::field_values(self)?
            .into_iter()
            .map(|field| WantedField {
                key: field.spec.key,
                section: field.section,
                leaf: field.leaf,
                value: crate::settings::format_field_value(field.spec.key, &field.value),
            })
            .collect();

        // The same renderings for what the file already says, so a leaf that is
        // saved with the value it already had is not rewritten at all.
        let current: HashMap<&str, String> = crate::settings::field_values(&base)?
            .into_iter()
            .map(|field| {
                (
                    field.spec.key,
                    crate::settings::format_field_value(field.spec.key, &field.value),
                )
            })
            .collect();
        let newline = if base_text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<String> = base_text.lines().map(str::to_string).collect();

        // Replace the value token of every leaf the file already carries. The
        // section is tracked rather than parsed per line because the schema is
        // exactly two levels deep.
        let mut section = String::new();
        let mut present: HashSet<&'static str> = HashSet::new();
        for line in lines.iter_mut() {
            let trimmed = line.trim();
            if let Some(name) = section_header(trimmed) {
                section = name.to_string();
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, _rest)) = trimmed.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let dotted = format!("{section}.{key}");
            let Some(field) = wanted.iter().find(|field| field.key == dotted) else {
                continue;
            };
            present.insert(field.key);
            if current.get(field.key).map(String::as_str) != Some(field.value.as_str()) {
                *line = format!("{key} = {}", field.value);
            }
        }

        // Both the missed keys and the missed sections are added last, walking
        // the sections backwards so an insertion never moves an index that a
        // section before it still has to use.
        let sections: Vec<&'static str> = wanted.iter().fold(Vec::new(), |mut sections, field| {
            if !sections.contains(&field.section) {
                sections.push(field.section);
            }
            sections
        });
        for position in (0..sections.len()).rev() {
            let section = sections[position];
            let header_present = lines
                .iter()
                .any(|line| section_header(line.trim()) == Some(section));
            let missing: Vec<&WantedField> = wanted
                .iter()
                .filter(|field| field.section == section && !present.contains(field.key))
                .collect();
            if header_present && missing.is_empty() {
                continue;
            }

            let mut block: Vec<String> = Vec::new();
            if !header_present {
                block.push(String::new());
                block.push(format!("[{section}]"));
            }
            block.extend(
                missing
                    .iter()
                    .map(|field| format!("{} = {}", field.leaf, field.value)),
            );

            let at = section_insert_index(&lines, &sections, position);
            lines.splice(at..at, block);
        }
        let mut text = lines.join(newline);
        if !text.ends_with(newline) {
            text.push_str(newline);
        }

        // Written beside the target and renamed over it: the rename is the only
        // step that touches the real file, so an interrupted save leaves the
        // previous contents exactly as they were instead of a truncated config.
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "config.toml".to_string());
        let temp = path.with_file_name(format!(".{file_name}.tmp"));
        fs::write(&temp, &text).with_context(|| format!("writing {}", temp.display()))?;
        if let Err(err) = fs::rename(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(anyhow::Error::new(err).context(format!("replacing {}", path.display())));
        }
        Ok(())
    }

    /// The text [`Self::save_preserving_comments`] patches, and the config it
    /// holds.
    fn patch_base(path: &Path, previous: Option<&str>) -> Result<(String, Self)> {
        if let Some(text) = previous
            && let Ok(config) = toml::from_str::<Self>(text)
        {
            return Ok((text.to_string(), config));
        }
        if previous.is_none()
            && let Ok(text) = fs::read_to_string(path)
            && let Ok(config) = toml::from_str::<Self>(&text)
        {
            return Ok((text, config));
        }
        let template = Self::generate_documented_template();
        let config = toml::from_str::<Self>(template)
            .context("the documented config template must parse")?;
        Ok((template.to_string(), config))
    }

    /// Generate human-readable TOML with comments, defaults, min and max values
    pub fn generate_documented_template() -> &'static str {
        r#"# ==============================================================================
# OsuMemoryReading - Configuration File
# A high-performance native Rust memory reader and drop-in tosu replacement.
# ==============================================================================

[server]
# Network interface host address to bind to.
# Default: "127.0.0.1" (localhost)
# Use "0.0.0.0" to allow other devices on your local network to access the overlay.
host = "127.0.0.1"

# Listening port for tosu HTTP and WebSocket replacement.
# Default: 24050
# Range: 1024 to 65535
port = 24050

# Allow Cross-Origin Resource Sharing (CORS) from any origin.
# Necessary for browser-based stream overlays (OBS, Streamlabs, Chrome).
# Default: true
cors_allow_all = true

# Enable real-time WebSocket streaming on ws://<host>:<port>/websocket/v2
# Default: true
enable_websocket = true

# Enable HTTP REST endpoints on http://<host>:<port>/json/v2 and /health
# Default: true
enable_http = true

# Which payload GET /json serves.
# "v1" is the gosumemory-compatible shape, which is what tosu serves on that
# path; "v2" is rtosu's own payload, which is what this route served before the
# parity work. /json/v1 always serves v1 and /json/v2 always serves v2, so this
# only decides the bare path.
# Options: "v1", "v2"
# Default: "v1"
json_payload = "v1"

# Serve browser overlays from a local directory.
# Every subfolder containing an index.html is treated as one overlay and served
# at http://<host>:<port>/overlays/<folder>/, which is the URL to paste into an
# OBS "Browser" source. Drop-in tosu v2 overlays work unmodified: the server
# injects a compatibility shim that points their API calls back at this server.
# Requires enable_http = true.
# Default: true
enable_overlays = true

# Directory holding one subfolder per browser overlay.
# Relative paths resolve against the current working directory.
# Default: "browser_overlays"
overlays_dir = "browser_overlays"

# Accept settings writes (POST /api/settings) only from the loopback interface.
# Readers on the LAN can always VIEW the landing page at http://<host>:<port>/
# and read GET /api/settings; they just cannot change anything. Moot when
# host = "127.0.0.1" (the default), which is the real boundary -- this toggle
# exists so host = "0.0.0.0" does not hand the settings over to every machine
# on the venue network.
# A convenience guard, not authentication: anything that can open a socket from
# the host machine is still inside it.
# Default: true
settings_write_local_only = true

# WebSocket per-socket initial write buffer size in bytes.
# Default: 65536 (64 KB)
ws_write_buffer_size = 65536

# WebSocket per-socket maximum write buffer size in bytes.
# Caps outbound buffering for slow or congested clients.
# Default: 524288 (512 KB)
ws_max_write_buffer_size = 524288

# WebSocket per-socket maximum frame size in bytes.
# Default: 16777216 (16 MB)
ws_max_frame_size = 16777216


[poll]
# Polling update frequency in Hertz (updates per second).
# 60 Hz = ~16.6 ms per update
# 120 Hz = ~8.3 ms per update
# Default: 60
# Range: 1 to 120 Hz
poll_rate_hz = 60

# Maximum memory scan search budget in Megabytes per osu! process.
# NOTE: This is NOT the RAM consumption of rtosu (rtosu runs at ~10 MB RAM).
# This is the memory search ceiling inside the osu! process virtual address space
# when scanning for memory signatures/pointers to avoid scanning unbound allocations.
# Default: 128 MB
# Range: 16 to 1024 MB
scan_budget_mb = 128

# Memory signature profile to load.
# Options: "tournament", "stable"
# Default: "tournament"
default_profile = "tournament"

# Automatically detect and switch between Single-Player Mode and Tournament Mode.
# When set to false, the reader locks exclusively to default_profile (e.g. "tournament"
# will only read tournament spectator/manager clients and ignore any background solo osu!).
# Default: true
auto_mode = true


[features]
# Extract and parse multiplayer tournament chat logs from memory.
# Default: true
enable_chat = true

# Enable real-time gradual PP calculation.
# Default: true
enable_pp = true

# Number of gradual PP checkpoints per beatmap.
# Range: 1 to 250
# Default: 100
# Set to 1 to only compute full-map difficulty (disables gradual live PP resolution for ultra-low CPU).

# Compute PP as if the NoFail mod were not on the play.
# For tournaments that force NF on every player and want the pp the play would
# have been worth without it. play.mods still reports NF, and star rating,
# accuracy, hits and rank are untouched -- only the pp values change.
# No-op on osu!taiko, where our calculator applies no NF penalty.
# Default: false
ignore_nf_for_pp = false

# Include the full hit error array in the JSON packet (packet.play.hitErrorArray).
# When set to false, hitErrorArray is sent as an empty array [] to save network bandwidth
# and JSON serialization overhead, while unstableRate is still accurately calculated and sent.
# Default: true
enable_hit_errors = true

[scoring]
# Weight a play's reported score by the mods it was set on.
# While this is true, the reported play.score, resultsScreen.score, every
# tourney.clients[].play.score, and tourney.totalScore.left/right (which becomes
# the sum of the weighted client scores) carry the factor below. Accuracy, rank,
# pp and the leaderboard are never weighted.
# Default: false
enable_mod_multipliers = false

# Mod acronym -> score factor. The keys are the acronyms reported in
# packet.play.mods.name, plus "NM" for a play with no mods; every key below is
# listed at 1.0, which leaves the in-game score unchanged. Change a value to
# weight that mod, e.g. "EZ" = 1.8 or "NF" = 0.5.
# A key naming more than one acronym is a mod osu! sets in one slot (Nightcore
# sets the DoubleTime bit as well), so "DT/NC" is a single factor rather than
# two; writing either acronym alone sets the same slot.
# Factors of different mods multiply, so HDHR with { "HD" = 1.05, "HR" = 1.1 }
# reports x1.155.
# Range per factor: 0.01 to 100.0
# Default: every mod at 1.0
mod_multipliers = { "NM" = 1.0, "NF" = 1.0, "EZ" = 1.0, "TD" = 1.0, "HD" = 1.0, "HR" = 1.0, "SD/PF" = 1.0, "DT/NC" = 1.0, "RX" = 1.0, "HT" = 1.0, "FL" = 1.0, "AT/CN" = 1.0, "SO" = 1.0, "AP" = 1.0, "4K" = 1.0, "5K" = 1.0, "6K" = 1.0, "7K" = 1.0, "8K" = 1.0, "FI" = 1.0, "RD" = 1.0, "TG" = 1.0, "9K" = 1.0, "10K" = 1.0, "1K" = 1.0, "3K" = 1.0, "2K" = 1.0, "v2" = 1.0, "MR" = 1.0 }




[logging]
# Logging verbosity level.
# Options: "trace", "debug", "info", "warn", "error"
# Default: "info"
# WARNING: "info" is recommended for regular production and tournament use.
# Setting this to "debug" or "trace" emits high-volume internal runtime logs
# which may increase CPU usage and impact high-frequency (60-120 Hz) poll timing.
level = "info"

# Save logs to daily files in the logs/ directory.
# Default: true
log_to_file = true

# Maximum number of daily log files to retain before pruning oldest.
# Default: 7
# Range: 1 to 365
max_log_files = 7
"#
    }
}

/// The section name of a `[section]` header line, if that is what the line is.
///
/// Deliberately strict: a comment that mentions a section, a dotted key, or an
/// inline table must not be mistaken for a header, because the writer uses this
/// to decide which section it is patching.
fn section_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() || inner.contains(['[', ']', '.', '=']) {
        return None;
    }
    Some(inner)
}

/// Where a block appended to `sections[position]` should land: just before the
/// header of the next section the text already carries, or at the end.
///
/// Walking back over the blank lines that separate the two sections keeps the
/// file's own spacing, so an appended key reads as the last line of its own
/// section rather than as the first line of the next one.
fn section_insert_index(lines: &[String], sections: &[&str], position: usize) -> usize {
    let mut index = lines.len();
    for later in &sections[position + 1..] {
        if let Some(found) = lines
            .iter()
            .position(|line| section_header(line.trim()) == Some(*later))
        {
            index = found;
            break;
        }
    }
    while index > 0 && lines[index - 1].trim().is_empty() {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, MAX_POLL_RATE_HZ};

    /// The poll rate is capped at 120 Hz, and both edges of the range are
    /// accepted while either side of them is rejected.
    ///
    /// The cap used to be 1000 Hz. osu! stable renders on the display's refresh,
    /// so a poll above 120 Hz re-reads memory that has not changed since the
    /// previous poll and pays for the syscall; the ceiling is there to keep a
    /// typo in a config file from spending a core on it.
    #[test]
    fn the_poll_rate_is_capped_at_the_games_own_refresh_rate() {
        assert_eq!(MAX_POLL_RATE_HZ, 120, "osu!stable's own ceiling");

        for hz in [1, 60, 120] {
            let mut config = AppConfig::default();
            config.poll.poll_rate_hz = hz;
            config
                .validate()
                .unwrap_or_else(|error| panic!("{hz} Hz should be accepted: {error}"));
            assert_eq!(config.poll_interval_ms(), 1000 / hz as u64, "{hz} Hz");
        }

        for hz in [0, 121, 1000] {
            let mut config = AppConfig::default();
            config.poll.poll_rate_hz = hz;
            let error = config
                .validate()
                .expect_err(&format!("{hz} Hz should be rejected"));
            assert!(
                error.to_string().contains("1 and 120 Hz"),
                "the message should name the range, got: {error}"
            );
        }
    }

    /// The shipped template's `[scoring]` table is the Rust default, key for
    /// key.
    ///
    /// This is the assertion that keeps the two halves of "the default is every
    /// mod at 1.0" honest. [`crate::scoring::default_multipliers`] builds the
    /// table from the slot list, while the template spells it out for a human
    /// to read; a new slot in one and not the other would be a key the file
    /// documents as absent or a value the file never shows.
    #[test]
    fn the_documented_template_lists_every_mod_at_one() {
        let template = AppConfig::generate_documented_template();
        assert!(
            template.contains("mod_multipliers = { \"NM\" = 1.0"),
            "the template must show the table, not an empty one"
        );

        let parsed: AppConfig = toml::from_str(template).expect("the shipped template must parse");
        parsed
            .validate()
            .expect("the shipped template must validate");

        assert!(
            !parsed.scoring.enable_mod_multipliers,
            "the feature ships off"
        );
        assert_eq!(
            parsed.scoring.mod_multipliers,
            crate::scoring::default_multipliers(),
            "the template and the Rust default must name the same keys"
        );
    }

    /// The `config.toml` committed at the repository root carries the same
    /// table, because it is the file an operator reads first.
    ///
    /// It is outside the crate, so nothing else would notice it drifting: a
    /// removed key would silently stop weighting a mod for anyone who copied
    /// that file.
    #[test]
    fn the_repository_config_carries_the_default_multiplier_table() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let parsed: AppConfig = toml::from_str(&raw).expect("the repository config must parse");
        parsed
            .validate()
            .expect("the repository config must validate");

        assert_eq!(
            parsed.scoring.mod_multipliers,
            crate::scoring::default_multipliers(),
            "{} must list every mod at 1.0",
            path.display()
        );
        assert!(!parsed.scoring.enable_mod_multipliers);
    }

    /// A bad table fails `config validate`, whether or not the feature is on.
    #[test]
    fn an_invalid_multiplier_table_fails_validation() {
        let mut config = AppConfig::default();
        config
            .scoring
            .mod_multipliers
            .insert("DTNC".to_string(), 2.0);
        let error = config
            .validate()
            .expect_err("an unknown acronym must be rejected");
        assert!(
            error.to_string().contains("unknown mod acronym 'DTNC'"),
            "got: {error}"
        );

        // Rejecting it while the feature is off is the point: a config that only
        // fails once a switch is flipped is a trap, and the shipped default
        // invites editing the table before enabling it.
        config.scoring.enable_mod_multipliers = false;
        config
            .validate()
            .expect_err("validation does not depend on the enable flag");

        // And a valid table resolves to the identity table while disabled.
        let config = AppConfig::default();
        assert!(
            config
                .scoring
                .resolved_multipliers()
                .expect("the default table is valid")
                .is_identity(),
            "disabled means identity, whatever the table says"
        );
    }

    /// Enabling the feature with a weighted table produces a table whose factor
    /// reaches the reader.
    #[test]
    fn an_enabled_table_resolves_to_its_factors() {
        let mut config = AppConfig::default();
        config.scoring.enable_mod_multipliers = true;
        config.scoring.mod_multipliers.insert("EZ".to_string(), 1.8);
        config.validate().expect("1.8 is in range");

        let multipliers = config
            .scoring
            .resolved_multipliers()
            .expect("the table resolves");
        assert_eq!(
            multipliers.apply(crate::client::mod_bits::EZ, 1_000_000),
            1_800_000
        );
        assert!(!multipliers.is_identity());
    }
}
