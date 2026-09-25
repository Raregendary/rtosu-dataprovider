use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const DEFAULT_CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub poll: PollConfig,
    pub features: FeatureConfig,
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
    /// Enable HTTP REST endpoints on /json/v2, /json, and /health
    pub enable_http: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PollConfig {
    /// Polling frequency in Hertz / frames per second (default: 60 Hz, min: 1 Hz, max: 1000 Hz)
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
    /// Decrypt XOR encrypted active mods in memory (HD, HR, DT, etc.)
    pub enable_mods_decryption: bool,
    /// Optional real-time PP calculation (requires feature 'rosu-mem' or 'pp')
    pub enable_pp: bool,
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
            enable_mods_decryption: true,
            enable_pp: false,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            log_to_file: false,
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
        if self.poll.poll_rate_hz == 0 || self.poll.poll_rate_hz > 1000 {
            anyhow::bail!(
                "poll.poll_rate_hz must be between 1 and 1000 Hz (got {})",
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


[poll]
# Polling update frequency in Hertz (updates per second).
# 60 Hz = ~16.6 ms per update
# 120 Hz = ~8.3 ms per update
# Default: 60
# Range: 1 to 1000 Hz
poll_rate_hz = 60

# Maximum memory scan search budget in Megabytes per osu! process.
# Default: 128 MB
# Range: 16 to 1024 MB
scan_budget_mb = 128

# Memory signature profile to load.
# Options: "tournament", "stable"
# Default: "tournament"
default_profile = "tournament"

# Automatically detect and switch between Single-Player Mode and Tournament Mode.
# Default: true
auto_mode = true


[features]
# Extract and parse multiplayer tournament chat logs from memory.
# Default: true
enable_chat = true

# Decode XOR-encrypted active mods in memory (HD, HR, DT, FL, AT, ScoreV2, etc.).
# Default: true
enable_mods_decryption = true

# Enable real-time PP calculation (requires 'pp' feature).
# Default: false
enable_pp = false


[logging]
# Logging verbosity level.
# Options: "trace", "debug", "info", "warn", "error"
# Default: "info"
# WARNING: "info" is recommended for regular production and tournament use.
# Setting this to "debug" or "trace" emits high-volume internal runtime logs
# which may increase CPU usage and impact high-frequency (60-120 Hz) poll timing.
level = "info"

# Save logs to daily files in the logs/ directory.
# Default: false
log_to_file = false

# Maximum number of daily log files to retain before pruning oldest.
# Default: 7
# Range: 1 to 365
max_log_files = 7
"#
    }
}
