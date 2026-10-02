//! The settings landing page at `/` and the JSON API behind it.
//!
//! tosu's equivalent is an Electron dashboard; rtosu has no window, so the
//! configuration has to be reachable from a browser instead. `GET /` renders a
//! page listing the current settings next to the discovered browser overlays,
//! `GET /api/settings` returns the same configuration as JSON plus the metadata
//! the page needs to render it, and `POST /api/settings` applies a flat patch of
//! `section.key` values.
//!
//! Three properties are worth stating up front, because they are what the code
//! below is shaped around:
//!
//! 1. **The field table is the only list of settings.** [`FIELD_SPECS`] is
//!    consumed by the editor form, by the patcher, and by the
//!    comment-preserving writer in [`crate::config`], so a setting cannot appear
//!    in one and be missing from another.
//! 2. **A patch is validated before anything happens.** It is merged into a
//!    clone of the live config and run through [`AppConfig::validate`], the same
//!    validator the startup path uses; on any error nothing is applied and
//!    nothing is written.
//! 3. **A write is local by default.** `server.settings_write_local_only`
//!    (default `true`) plus a loopback `Origin` check keep a LAN viewer's browser
//!    from rewriting the config of the machine running the tournament. It is a
//!    convenience guard, not authentication: see [`write_refusal`].

use crate::config::{AppConfig, MAX_POLL_RATE_HZ};
use crate::overlays::{self, Overlay, escape_html};
use crate::scoring;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::watch;

/// Largest `POST /api/settings` body accepted, in bytes.
///
/// The only legal payload is a few hundred bytes of `key: value` pairs; the cap
/// exists so a stray upload cannot be parsed into memory at all. Anything over
/// it is answered `413` by the router's body limit.
pub const MAX_PATCH_BYTES: usize = 64 * 1024;

/// What kind of value a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Bool,
    Integer,
    Float,
    String,
    /// A table of mod acronym to factor (`scoring.mod_multipliers`).
    Multipliers,
}

/// One setting: the editor's row, the patcher's contract, and the writer's list
/// of leaves, all from the same entry.
#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec {
    /// Dot path, `section.key`, which is also the key a patch uses.
    pub key: &'static str,
    pub kind: FieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Accepted values for a `String` field, empty when it is free text.
    pub options: &'static [&'static str],
    /// Persisted but only read at startup: the page says so instead of
    /// pretending the change is live.
    pub restart_required: bool,
    /// Human label for the form. The key is shown beside it.
    pub label: &'static str,
}

const fn toggle(key: &'static str, label: &'static str, restart_required: bool) -> FieldSpec {
    FieldSpec {
        key,
        kind: FieldKind::Bool,
        min: None,
        max: None,
        options: &[],
        restart_required,
        label,
    }
}

const fn number(
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    restart_required: bool,
) -> FieldSpec {
    FieldSpec {
        key,
        kind: FieldKind::Integer,
        min: Some(min),
        max: Some(max),
        options: &[],
        restart_required,
        label,
    }
}

const fn text(
    key: &'static str,
    label: &'static str,
    options: &'static [&'static str],
    restart_required: bool,
) -> FieldSpec {
    FieldSpec {
        key,
        kind: FieldKind::String,
        min: None,
        max: None,
        options,
        restart_required,
        label,
    }
}

const fn table(key: &'static str, label: &'static str, restart_required: bool) -> FieldSpec {
    FieldSpec {
        key,
        kind: FieldKind::Multipliers,
        min: None,
        max: None,
        options: &[],
        restart_required,
        label,
    }
}

/// Every setting the landing page can show and `POST /api/settings` can change,
/// in the order the config file and the form present them.
///
/// The `restart_required` flags follow one rule: a value the startup path reads
/// once -- the bind address, the signature-scan budget, the log subscriber, the
/// profile the sessions were built from -- needs a restart, and everything the
/// poll loop re-reads per tick does not.
pub const FIELD_SPECS: &[FieldSpec] = &[
    text("server.host", "Host", &[], true),
    number("server.port", "Port", 1024.0, 65535.0, true),
    toggle("server.cors_allow_all", "Permissive CORS", true),
    toggle("server.enable_websocket", "WebSocket stream", true),
    toggle("server.enable_http", "HTTP API", true),
    text(
        "server.json_payload",
        "Payload served on /json",
        &["v1", "v2"],
        true,
    ),
    toggle("server.enable_overlays", "Browser overlays", true),
    text("server.overlays_dir", "Overlay directory", &[], true),
    toggle(
        "server.settings_write_local_only",
        "Accept settings writes from localhost only",
        false,
    ),
    number(
        "poll.poll_rate_hz",
        "Poll rate (Hz)",
        1.0,
        MAX_POLL_RATE_HZ as f64,
        false,
    ),
    number(
        "poll.scan_budget_mb",
        "Memory scan budget (MB)",
        16.0,
        2048.0,
        true,
    ),
    text(
        "poll.default_profile",
        "Memory profile",
        &["tournament", "stable"],
        true,
    ),
    toggle(
        "poll.auto_mode",
        "Auto-detect single-player / tournament",
        true,
    ),
    toggle("features.enable_chat", "Tournament chat attribution", false),
    toggle("features.enable_pp", "Real-time PP calculation", false),
    toggle("features.ignore_nf_for_pp", "Ignore NoFail for PP", false),
    toggle("features.enable_hit_errors", "Hit error array", false),
    toggle(
        "scoring.enable_mod_multipliers",
        "Mod score multipliers",
        false,
    ),
    table("scoring.mod_multipliers", "Per-mod score factors", false),
    text(
        "logging.level",
        "Log level",
        &["trace", "debug", "info", "warn", "error"],
        true,
    ),
    toggle("logging.log_to_file", "Write daily log files", true),
    number("logging.max_log_files", "Log files kept", 1.0, 365.0, true),
];

/// The spec for one key, if the page knows about it.
pub fn field_spec(key: &str) -> Option<&'static FieldSpec> {
    FIELD_SPECS.iter().find(|spec| spec.key == key)
}

/// The keys a patch may change but that only take effect on the next start.
pub fn restart_required_keys() -> Vec<&'static str> {
    FIELD_SPECS
        .iter()
        .filter(|spec| spec.restart_required)
        .map(|spec| spec.key)
        .collect()
}

/// Everything the poll loop reads from the settings: cheap to clone, and
/// published on every accepted patch.
///
/// Deliberately not the whole `AppConfig`. A value the startup path consumed
/// once -- the bind address, the log subscriber, the signature-scan budget -- is
/// absent here on purpose: adopting one mid-run would report a change the
/// process has not actually made, and the page marks those keys *restart*
/// instead.
#[derive(Clone, Debug)]
pub struct LiveSettings {
    pub poll_interval: Duration,
    pub enable_pp: bool,
    pub enable_hit_errors: bool,
    pub enable_chat: bool,
    pub ignore_nf_for_pp: bool,
    pub mod_multipliers: Arc<scoring::ModMultipliers>,
}

impl LiveSettings {
    /// The live subset of `config`.
    pub fn from_config(config: &AppConfig) -> Result<Self> {
        Ok(Self {
            poll_interval: Duration::from_millis(config.poll_interval_ms()),
            enable_pp: config.features.enable_pp,
            enable_hit_errors: config.features.enable_hit_errors,
            enable_chat: config.features.enable_chat,
            ignore_nf_for_pp: config.features.ignore_nf_for_pp,
            mod_multipliers: Arc::new(config.scoring.resolved_multipliers()?),
        })
    }
}

/// A validated patch, before anything has been written.
#[derive(Debug, Clone)]
pub struct PatchOutcome {
    /// The keys the patch changed, sorted.
    pub applied: Vec<String>,
    /// The subset of `applied` that needs a restart, sorted.
    pub restart_required: Vec<String>,
    /// The merged configuration, which is what a successful save writes.
    pub config: AppConfig,
}

/// The configuration, the file it lives in, and the live view of it.
///
/// Held by the server (for the page and the API) and by the poll loop (for the
/// live receiver), which is why it is shared as an `Arc`.
pub struct SettingsStore {
    path: PathBuf,
    config: RwLock<AppConfig>,
    live_tx: watch::Sender<LiveSettings>,
}

impl SettingsStore {
    /// Build a store for the config at `path`.
    pub fn new(path: impl Into<PathBuf>, config: AppConfig) -> Result<Self> {
        let live = LiveSettings::from_config(&config)?;
        let (live_tx, _) = watch::channel(live);
        Ok(Self {
            path: path.into(),
            config: RwLock::new(config),
            live_tx,
        })
    }

    /// The file the page reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current configuration.
    pub fn config(&self) -> AppConfig {
        match self.config.read() {
            Ok(guard) => guard.clone(),
            // A poisoned lock means a panic happened while it was held. The
            // value is a plain config clone that cannot be half-written, so
            // recovering it is strictly better than pretending there is no
            // configuration at all.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// A receiver for the live subset, for the poll loop.
    pub fn subscribe(&self) -> watch::Receiver<LiveSettings> {
        self.live_tx.subscribe()
    }

    /// The live subset as it stands.
    pub fn live(&self) -> LiveSettings {
        self.live_tx.borrow().clone()
    }

    /// Whether the config file can be written right now, and why not when it
    /// cannot. Probed per request rather than cached, so a file that becomes
    /// read-only after startup is reported honestly.
    pub fn check_writable(&self) -> std::result::Result<(), String> {
        probe_writable(&self.path)
    }

    /// Validate a flat patch against a clone of the current config.
    ///
    /// Returns the merged config plus which keys need a restart, and touches no
    /// file and no shared state -- so a caller that fails a later step can
    /// abandon the result with nothing to undo.
    pub fn apply_patch(&self, patch: &serde_json::Value) -> Result<PatchOutcome> {
        let object = patch.as_object().ok_or_else(|| {
            anyhow::anyhow!("the patch must be a JSON object of \"section.key\": value pairs")
        })?;
        if object.is_empty() {
            bail!("the patch is empty; there is nothing to change");
        }

        let mut merged = self.config();
        let mut applied: Vec<String> = Vec::with_capacity(object.len());
        let mut restart_required: Vec<String> = Vec::new();
        for (key, value) in object {
            let spec = field_spec(key).ok_or_else(|| anyhow::anyhow!("unknown key '{key}'"))?;
            set_field(&mut merged, spec, value).map_err(|err| keyed(key, err))?;
            validate_field(&merged, spec).map_err(|err| keyed(key, err))?;
            applied.push(key.clone());
            if spec.restart_required {
                restart_required.push(key.clone());
            }
        }

        // The cross-field rules (an overlay directory with HTTP disabled, say)
        // live in the one validator startup uses, so a patch cannot satisfy the
        // page and then fail to load on the next start.
        merged
            .validate()
            .map_err(|err| anyhow::anyhow!("{}", attribute_error(&err, &applied)))?;

        applied.sort();
        restart_required.sort();
        Ok(PatchOutcome {
            applied,
            restart_required,
            config: merged,
        })
    }

    /// Write `config` to the store's file, preserving every comment.
    pub fn persist(&self, config: &AppConfig) -> Result<()> {
        config.save_preserving_comments(&self.path, None)
    }

    /// Make `config` the live one and publish the changed live subset.
    ///
    /// Called after the file was written, so if this fails the disk is ahead of
    /// memory: the next start reads the new file, which is the recoverable
    /// direction of the two.
    pub fn commit(&self, config: AppConfig) -> Result<()> {
        let live = LiveSettings::from_config(&config)?;
        let mut guard = self
            .config
            .write()
            .map_err(|_| anyhow::anyhow!("the settings lock is poisoned"))?;
        *guard = config;
        drop(guard);
        // A send error only means every receiver is gone (the poll loop has
        // exited), which is not a failure of the save.
        let _ = self.live_tx.send(live);
        Ok(())
    }
}

/// Whether the file at `path` can be written. The error text is what the page
/// shows when it cannot.
///
/// A missing file counts as writable when its directory is: the writer starts
/// from the documented template and creates it.
fn probe_writable(path: &Path) -> std::result::Result<(), String> {
    match fs::OpenOptions::new().append(true).open(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let dir = match path.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir,
                _ => Path::new("."),
            };
            let probe = dir.join(".rtosu-config-write-probe");
            match fs::write(&probe, b"") {
                Ok(()) => {
                    let _ = fs::remove_file(&probe);
                    Ok(())
                }
                Err(err) => Err(err.to_string()),
            }
        }
        Err(err) => Err(err.to_string()),
    }
}

/// Name the key a whole-config validation error belongs to.
///
/// `AppConfig::validate` messages start with the offending key for most rules,
/// so the first applied key whose name appears in the message wins; for a
/// cross-field rule that names two keys, the first changed key is the honest
/// attribution.
fn attribute_error(err: &anyhow::Error, applied: &[String]) -> String {
    let message = format!("{err:#}");
    for key in applied {
        if message.contains(key.as_str()) {
            return keyed_message(key, &message);
        }
    }
    match applied.first() {
        Some(key) => keyed_message(key, &message),
        None => message,
    }
}

/// Attach a key name to a field error, unless the message already carries it.
///
/// [`scoring::ModMultipliers::new`] and `AppConfig::validate` both name their own
/// key, so prefixing unconditionally would print it twice -- and the page shows
/// the server's message verbatim.
fn keyed(key: &str, err: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("{}", keyed_message(key, &format!("{err:#}")))
}

fn keyed_message(key: &str, message: &str) -> String {
    if message.starts_with(key) {
        message.to_string()
    } else {
        format!("{key}: {message}")
    }
}

/// One field with the value the config currently holds for it.
#[derive(Debug, Clone)]
pub struct FieldValue {
    pub spec: &'static FieldSpec,
    pub section: &'static str,
    pub leaf: &'static str,
    pub value: toml::Value,
}

impl FieldValue {
    /// The dot path, which is what a patch uses as its key.
    pub fn key(&self) -> &'static str {
        self.spec.key
    }
}

/// Every field, in the field table's order, with the value `config` holds.
///
/// One serialisation for the whole table: the editor form and the config writer
/// both need every value, and re-serialising per key would be 22 passes for no
/// reason. A key the config does not carry is an error rather than a silently
/// skipped row, because a dropped row in the writer would drop a setting from
/// the file.
pub fn field_values(config: &AppConfig) -> Result<Vec<FieldValue>> {
    let root = config_as_toml(config)?;
    let mut values = Vec::with_capacity(FIELD_SPECS.len());
    for spec in FIELD_SPECS {
        let (section, leaf) = spec
            .key
            .split_once('.')
            .with_context(|| format!("'{}' is not a section.key path", spec.key))?;
        let value = root
            .get(section)
            .and_then(|table| table.get(leaf))
            .cloned()
            .with_context(|| format!("the config carries no {}", spec.key))?;
        values.push(FieldValue {
            spec,
            section,
            leaf,
            value,
        });
    }
    Ok(values)
}

/// The config as a `toml::Value`.
///
/// Serialise-then-reparse is what lets one leaf be read or written by path
/// without a match arm per key, which is the property that keeps
/// [`FIELD_SPECS`] the only list of settings in the codebase.
fn config_as_toml(config: &AppConfig) -> Result<toml::Value> {
    let text = toml::to_string(config).context("serialising the config")?;
    toml::from_str::<toml::Value>(&text).context("re-parsing the serialised config")
}

/// Apply one JSON value to the config, through its spec.
fn set_field(config: &mut AppConfig, spec: &FieldSpec, value: &serde_json::Value) -> Result<()> {
    let (section, leaf) = spec
        .key
        .split_once('.')
        .with_context(|| format!("'{}' is not a section.key path", spec.key))?;

    let mut root = config_as_toml(config)?;
    let table = root
        .get_mut(section)
        .and_then(toml::Value::as_table_mut)
        .with_context(|| format!("the config has no [{section}] table"))?;
    table.insert(leaf.to_string(), json_to_toml(spec, value)?);

    let text = toml::to_string(&root).context("serialising the patched config")?;
    *config = toml::from_str::<AppConfig>(&text).context("deserialising the patched config")?;
    Ok(())
}

/// What only a field itself can be checked for.
///
/// Types and ranges are enforced by [`json_to_toml`]; the acronym table is the
/// one value whose validity depends on the scoring module rather than on the
/// field table, and it is checked here so the error names its key instead of
/// coming back as an anonymous whole-config failure.
fn validate_field(config: &AppConfig, spec: &FieldSpec) -> Result<()> {
    if spec.kind == FieldKind::Multipliers {
        scoring::ModMultipliers::new(&config.scoring.mod_multipliers)?;
    }
    Ok(())
}

/// Convert one JSON value to the TOML value its spec asks for, rejecting
/// anything the field table does not allow.
fn json_to_toml(spec: &FieldSpec, value: &serde_json::Value) -> Result<toml::Value> {
    match spec.kind {
        FieldKind::Bool => value
            .as_bool()
            .map(toml::Value::Boolean)
            .ok_or_else(|| anyhow::anyhow!("expected true or false, got {value}")),
        FieldKind::Integer => {
            let number = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("expected a whole number, got {value}"))?;
            check_range(spec, number as f64)?;
            Ok(toml::Value::Integer(number))
        }
        FieldKind::Float => {
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| anyhow::anyhow!("expected a finite number, got {value}"))?;
            check_range(spec, number)?;
            Ok(toml::Value::Float(number))
        }
        FieldKind::String => {
            let text = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("expected a string, got {value}"))?;
            if !spec.options.is_empty() && !spec.options.contains(&text) {
                bail!("must be one of {} (got '{text}')", spec.options.join(", "));
            }
            Ok(toml::Value::String(text.to_string()))
        }
        FieldKind::Multipliers => {
            let object = value.as_object().ok_or_else(|| {
                anyhow::anyhow!("expected an object of \"ACRONYM\": factor, got {value}")
            })?;
            let mut table = toml::map::Map::new();
            for (key, factor) in object {
                let factor = factor
                    .as_f64()
                    .filter(|factor| factor.is_finite())
                    .ok_or_else(|| {
                        anyhow::anyhow!("'{key}' must be a finite number, got {factor}")
                    })?;
                if !(scoring::MIN_FACTOR..=scoring::MAX_FACTOR).contains(&factor) {
                    bail!(
                        "'{key}' must be between {} and {} (got {})",
                        format_float(scoring::MIN_FACTOR),
                        format_float(scoring::MAX_FACTOR),
                        format_float(factor)
                    );
                }
                table.insert(key.clone(), toml::Value::Float(factor));
            }
            Ok(toml::Value::Table(table))
        }
    }
}

/// Enforce a field's declared range.
fn check_range(spec: &FieldSpec, value: f64) -> Result<()> {
    if let Some(min) = spec.min
        && value < min
    {
        bail!(
            "must be at least {} (got {})",
            format_float(min),
            format_float(value)
        );
    }
    if let Some(max) = spec.max
        && value > max
    {
        bail!(
            "must be at most {} (got {})",
            format_float(max),
            format_float(value)
        );
    }
    Ok(())
}

/// Render one field's value the way the config file writes it.
///
/// Floats always keep a decimal point (`1.0`, not `1`) and the multiplier table
/// keeps its keys quoted and in the slot order the template uses, so a value that
/// is saved unchanged reproduces the shipped line byte for byte.
pub fn format_field_value(key: &str, value: &toml::Value) -> String {
    if key == "scoring.mod_multipliers"
        && let Some(table) = value.as_table()
    {
        let mut factors: HashMap<String, f64> = HashMap::with_capacity(table.len());
        for (name, factor) in table {
            factors.insert(name.clone(), toml_float(factor));
        }
        let body = ordered_multiplier_keys(&factors)
            .into_iter()
            .map(|name| {
                let factor = factors.get(&name).copied().unwrap_or(1.0);
                format!("\"{name}\" = {}", format_float(factor))
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{{ {body} }}");
    }
    match value {
        toml::Value::Float(number) => format_float(*number),
        other => other.to_string(),
    }
}

/// The multiplier table as the page's editor shows it: one key per line, in the
/// order the config file writes them.
pub fn format_multipliers_pretty(table: &HashMap<String, f64>) -> String {
    let mut out = String::from("{");
    for (index, key) in ordered_multiplier_keys(table).iter().enumerate() {
        let factor = table.get(key).copied().unwrap_or(1.0);
        out.push_str(if index == 0 { "\n" } else { ",\n" });
        out.push_str(&format!("  \"{key}\": {}", format_float(factor)));
    }
    out.push_str("\n}");
    out
}

/// Every key of a multiplier table, in the order the config file writes them:
/// `NM` first, then each mod slot (the group form and its parts), then anything
/// the slot list does not know -- which validation rejects, but which the page
/// still has to show rather than silently drop.
fn ordered_multiplier_keys(table: &HashMap<String, f64>) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::with_capacity(table.len());
    let push = |key: &str, ordered: &mut Vec<String>| {
        if table.contains_key(key) && !ordered.iter().any(|known| known == key) {
            ordered.push(key.to_string());
        }
    };
    push(scoring::NO_MOD_KEY, &mut ordered);
    for slot in scoring::MOD_SLOTS {
        push(slot.key, &mut ordered);
        for part in slot.key.split('/') {
            push(part, &mut ordered);
        }
    }
    let mut rest: Vec<String> = table
        .keys()
        .filter(|key| !ordered.contains(key))
        .cloned()
        .collect();
    rest.sort();
    ordered.extend(rest);
    ordered
}

/// A float the way TOML writes it, with a decimal point so it can never be read
/// back as an integer.
fn format_float(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

/// A numeric TOML value as an `f64`, for the places where the file may hold an
/// integer where a float is expected.
fn toml_float(value: &toml::Value) -> f64 {
    match value {
        toml::Value::Float(number) => *number,
        toml::Value::Integer(number) => *number as f64,
        _ => 1.0,
    }
}

/// Why a viewer may not write, as a value the page can switch on.
///
/// Serialises to the reason strings `GET /api/settings` documents:
/// `"settings_write_local_only"` and `"config_read_only"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteBlocker {
    /// The viewer is not on the machine running the server.
    SettingsWriteLocalOnly,
    /// The config file cannot be opened for writing.
    ConfigReadOnly,
}

impl WriteBlocker {
    pub fn code(self) -> &'static str {
        match self {
            Self::SettingsWriteLocalOnly => "settings_write_local_only",
            Self::ConfigReadOnly => "config_read_only",
        }
    }
}

/// The one place that decides whether a viewer may write, and why not.
///
/// Three checks, and each of them earns its place:
///
/// * `server.settings_write_local_only` (default `true`) refuses a peer that is
///   not on the loopback interface, which is what keeps a LAN-published
///   instance from handing the configuration to the venue network.
/// * The same guard refuses a request carrying a **foreign `Origin`**, because
///   `cors_allow_all` answers a preflight from any origin: without this, a page
///   loaded on a *viewing* machine could issue the `POST` and read the reply.
///   No `Origin` at all is allowed -- that is a local tool, not a browser.
/// * A config file that cannot be opened for writing is refused, so a change is
///   never accepted and then lost on the next start.
///
/// Deliberately not authentication: anything that can open a socket from the
/// host machine is inside the guard.
pub fn write_blocker(
    config: &AppConfig,
    peer_is_loopback: bool,
    origin: Option<&str>,
    file_writable: std::result::Result<(), String>,
) -> Option<WriteBlocker> {
    if config.server.settings_write_local_only {
        if !peer_is_loopback {
            return Some(WriteBlocker::SettingsWriteLocalOnly);
        }
        if let Some(origin) = origin
            && !origin_is_loopback(origin)
        {
            return Some(WriteBlocker::SettingsWriteLocalOnly);
        }
    }
    if file_writable.is_err() {
        return Some(WriteBlocker::ConfigReadOnly);
    }
    None
}

/// The host part of an `Origin` header, or `None` when it is not an HTTP(S)
/// origin at all.
///
/// A literal `null` (sent by a sandboxed document) and a non-URL value both land
/// here as `None`, and callers must treat that as foreign rather than local.
pub fn origin_host(origin: &str) -> Option<String> {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = match authority.strip_prefix('[') {
        // IPv6 literal, e.g. `[::1]:24050`.
        Some(rest) => rest.split(']').next()?.to_string(),
        None => authority
            .rsplit_once(':')
            .map(|(host, _port)| host.to_string())
            .unwrap_or_else(|| authority.to_string()),
    };
    if host.is_empty() { None } else { Some(host) }
}

/// Whether an `Origin` header names the loopback interface.
pub fn origin_is_loopback(origin: &str) -> bool {
    matches!(
        origin_host(origin)
            .map(|host| host.to_ascii_lowercase())
            .as_deref(),
        Some("127.0.0.1") | Some("localhost") | Some("::1")
    )
}

/// The `GET /api/settings` body.
///
/// `config` is `AppConfig` as it stands, serialised under its TOML field names,
/// so the JSON shape *is* the config shape and the page needs no translation
/// table.
#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub version: String,
    pub config_path: String,
    pub writable: bool,
    pub writable_reason: Option<WriteBlocker>,
    /// Why the file itself cannot be written, when it cannot: the detail behind
    /// a `config_read_only` reason, which is not a stable code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable_detail: Option<String>,
    pub restart_required_keys: Vec<&'static str>,
    pub fields: &'static [FieldSpec],
    pub config: AppConfig,
}

/// Build the `GET /api/settings` body for one viewer.
pub fn settings_response(store: &SettingsStore, peer_is_loopback: bool) -> SettingsResponse {
    let config = store.config();
    let file_writable = store.check_writable();
    let blocker = write_blocker(&config, peer_is_loopback, None, file_writable.clone());
    SettingsResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        config_path: store.path().display().to_string(),
        writable: blocker.is_none(),
        writable_reason: blocker,
        writable_detail: file_writable.err(),
        restart_required_keys: restart_required_keys(),
        fields: FIELD_SPECS,
        config,
    }
}

/// The project's source repository, shown in the header and footer.
const GITHUB_URL: &str = "https://github.com/Raregendary/rtosu-dataprovider";

/// The original tosu project, credited in the footer: this server exists to
/// replace its data API, and drop-in overlays are written against tosu.
const TOSU_URL: &str = "https://tosu.app/";

/// Everything `/` renders.
pub struct LandingView<'a> {
    /// The bound host, as the listener reported it.
    pub host: &'a str,
    pub port: u16,
    /// The file the page edits, when there is one.
    pub config_path: Option<&'a Path>,
    /// The settings section, from [`settings_response`]. `None` renders the page
    /// with the settings area replaced by a notice.
    pub settings: Option<SettingsResponse>,
    /// The discovered overlays and the directory they came from.
    pub overlays: Option<(&'a [Overlay], &'a Path)>,
    /// Whether this viewer may hot-restart the server: only the CLI `serve`
    /// process has a supervisor to do it, and only a loopback viewer passes
    /// the endpoint's guard, so the button is offered under exactly the
    /// conditions the endpoint would honour.
    pub can_restart: bool,
}

/// Render the landing page at `/`.
///
/// One hand-written string with no build step, no framework and no request to
/// anything but this server, so it works on a venue machine with no internet
/// and cannot break because a CDN is down.
pub fn landing_html(view: &LandingView<'_>) -> String {
    let mut html = String::with_capacity(32 * 1024);
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>rtosu-dataprovider</title>\n<style>\n");
    html.push_str(LANDING_STYLE);
    // The overlay cards keep the styling they have on /overlays, so a card looks
    // the same wherever it is shown.
    html.push_str(overlays::cards_style());
    html.push_str("\n</style>\n</head>\n<body>\n");
    html.push_str(&header_html(view));
    html.push_str(&banner_html(view));
    // One column, top to bottom: settings first (it is the working surface),
    // overlays next at full width so the cards read like the /overlays
    // dashboard rather than cramped sidebar widgets, then the endpoint index.
    html.push_str("<main class=\"wrap\">\n");
    html.push_str(&settings_html(view));
    html.push_str(&overlays_html(view));
    html.push_str(&links_html());
    html.push_str(&logs_html(false));
    html.push_str("</main>\n");
    html.push_str(&json_inspector_html());
    html.push_str(&footer_html());
    html.push_str("<script id=\"rtosu-data\" type=\"application/json\">");
    html.push_str(&page_meta_json(view));
    html.push_str("</script>\n<script>\n");
    html.push_str(LANDING_SCRIPT);
    html.push_str("</script>\n</body>\n</html>");
    html
}

/// Render the dedicated standalone log viewer page at `/logs`.
pub fn logs_page_html(view: &LandingView<'_>) -> String {
    let mut html = String::with_capacity(32 * 1024);
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>rtosu-dataprovider - System Logs</title>\n<style>\n");
    html.push_str(LANDING_STYLE);
    html.push_str("\n</style>\n</head>\n<body>\n");
    html.push_str(&header_html(view));
    html.push_str("<main class=\"wrap logs-standalone-wrap\">\n");
    html.push_str(&logs_html(true));
    html.push_str("</main>\n");
    html.push_str(&json_inspector_html());
    html.push_str(&footer_html());
    html.push_str("<script id=\"rtosu-data\" type=\"application/json\">");
    html.push_str(&page_meta_json(view));
    html.push_str("</script>\n<script>\n");
    html.push_str(LANDING_SCRIPT);
    html.push_str("</script>\n</body>\n</html>");
    html
}

fn header_html(view: &LandingView<'_>) -> String {
    let version = view
        .settings
        .as_ref()
        .map(|settings| settings.version.clone())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let host = if view.host.is_empty() || view.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        view.host
    };
    let mut html = String::from("<header class=\"top\"><div class=\"top-inner\">\n");
    html.push_str(
        "<span class=\"brand\"><span class=\"mark\">R</span>rtosu<span class=\"dot\">&middot;</span>dataprovider</span>\n",
    );
    html.push_str(&format!(
        "<span class=\"pill\">v{}</span>\n",
        escape_html(&version)
    ));
    html.push_str(&format!(
        "<a class=\"pill\" href=\"http://{}:{}/\" title=\"this page\">{}:{}</a>\n",
        escape_html(host),
        view.port,
        escape_html(host),
        view.port
    ));
    if let Some(path) = view.config_path {
        html.push_str(&format!(
            "<span class=\"pill\" title=\"the file this page reads and writes\">{}</span>\n",
            escape_html(&path.display().to_string())
        ));
    }
    html.push_str("<span class=\"top-actions\">\n");
    if view.can_restart {
        html.push_str(
            "<button class=\"ghost danger\" id=\"restart\" type=\"button\" title=\"Restart the server so settings that need a restart take effect. Live overlay sockets drop for a moment.\">&#x21bb; Restart</button>\n",
        );
    }
    html.push_str(&format!(
        "<a class=\"ghost\" href=\"{GITHUB_URL}\" target=\"_blank\" rel=\"noreferrer noopener\">GitHub</a>\n",
    ));
    html.push_str("</span>\n</div></header>\n");
    html
}

/// The status banner: the read-only reason when writes are refused, and the
/// restart notice the page's script writes into it after a save.
fn banner_html(view: &LandingView<'_>) -> String {
    let can_write = view
        .settings
        .as_ref()
        .is_some_and(|settings| settings.writable);
    let (class, text) = match &view.settings {
        None => (
            "warn",
            "Settings are unavailable in this mode: no config file is attached, so nothing \
             here can be changed."
                .to_string(),
        ),
        Some(_) if can_write => ("ok", String::new()),
        Some(settings) => match settings.writable_reason {
            Some(WriteBlocker::ConfigReadOnly) => (
                "err",
                match &settings.writable_detail {
                    Some(detail) => format!(
                        "Read-only view: the config file could not be opened for writing ({detail})."
                    ),
                    None => "Read-only view: the config file is not writable.".to_string(),
                },
            ),
            _ => (
                "warn",
                "Read-only view: settings writes are restricted to localhost \
                 (server.settings_write_local_only). You can look at everything here, but only \
                 the machine running the server can save."
                    .to_string(),
            ),
        },
    };
    format!(
        "<div class=\"wrap tight\"><div class=\"banner {class}\" id=\"banner\"{}>{}</div></div>\n",
        if can_write { " hidden" } else { "" },
        escape_html(&text)
    )
}

/// What the page's script needs to know, embedded in the page so it never has to
/// ask the server for it.
#[derive(Serialize)]
struct PageMeta {
    version: String,
    writable: bool,
    writable_reason: Option<&'static str>,
    config_path: Option<String>,
    restart_required: Vec<&'static str>,
    settings_available: bool,
    restart_available: bool,
}

fn page_meta_json(view: &LandingView<'_>) -> String {
    let meta = PageMeta {
        version: view
            .settings
            .as_ref()
            .map(|settings| settings.version.clone())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
        writable: view
            .settings
            .as_ref()
            .is_some_and(|settings| settings.writable),
        writable_reason: view
            .settings
            .as_ref()
            .and_then(|settings| settings.writable_reason)
            .map(WriteBlocker::code),
        config_path: view.config_path.map(|path| path.display().to_string()),
        restart_required: restart_required_keys(),
        settings_available: view.settings.is_some(),
        restart_available: view.can_restart,
    };
    // `<` is escaped because the blob sits inside a `<script>` element, where a
    // `</script>` in a path would end the element early. A JSON parser reads
    // `\u003c` back as `<`.
    serde_json::to_string(&meta)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c")
}

/// A readable name for a config section.
fn section_title(section: &str) -> &str {
    match section {
        "server" => "Server",
        "poll" => "Reader",
        "features" => "Features",
        "scoring" => "Scoring",
        "logging" => "Logging",
        other => other,
    }
}

/// One line about what a section is for.
fn section_note(section: &str) -> &'static str {
    match section {
        "server" => "The listener and the pages it serves.",
        "poll" => "How often the game's memory is read, and how much of it is scanned.",
        "features" => "What the payload carries. These apply from the next poll.",
        "scoring" => "Optional per-mod weighting of the reported score.",
        "logging" => "Log verbosity and rotation.",
        _ => "",
    }
}

/// The settings form, grouped by section, rendered from [`FIELD_SPECS`] so a new
/// setting cannot appear in the config without appearing here.
fn settings_html(view: &LandingView<'_>) -> String {
    let Some(settings) = &view.settings else {
        return "<section class=\"panel\">\n<h2>Settings</h2>\n<p class=\"hint\">This server was \
                started without a config file, so there is nothing to show.</p>\n</section>\n"
            .to_string();
    };
    let fields = match field_values(&settings.config) {
        Ok(fields) => fields,
        Err(err) => {
            return format!(
                "<section class=\"panel\">\n<h2>Settings</h2>\n<p class=\"hint\">Could not read \
                 the configuration: {}</p>\n</section>\n",
                escape_html(&format!("{err:#}"))
            );
        }
    };

    let mut html = String::from("<section class=\"panel\">\n<div class=\"panel-head\">\n");
    html.push_str("<h2>Settings</h2>\n");
    html.push_str(
        "<p class=\"hint\">Changes are written back to the config file with every comment and \
         every unedited line preserved. A value marked <span class=\"tag\">restart</span> is \
         saved immediately but only takes effect on the next start -- use <em>Restart</em> in \
         the header to apply it now. Click a section heading to collapse it.</p>\n",
    );
    html.push_str("</div>\n<form id=\"settings\" autocomplete=\"off\">\n");

    let mut section = "";
    let mut open = false;
    for field in &fields {
        if field.section != section {
            if open {
                html.push_str("</div>\n</details>\n");
            }
            // `details`/`summary` rather than `fieldset`/`legend`: the section
            // collapses with no script at all, the summary keeps the title,
            // the `[section]` key and the one-line note on a single readable
            // row, and the page's script remembers which ones the operator
            // left open.
            html.push_str(&format!(
                "<details class=\"section\" id=\"sec-{}\">\n<summary>\
<span class=\"chev\" aria-hidden=\"true\"></span>\
<span class=\"s-title\">{}</span>\
<span class=\"key\">[{}]</span>\
<span class=\"s-note\">{}</span>\
</summary>\n",
                escape_html(field.section),
                escape_html(section_title(field.section)),
                escape_html(field.section),
                escape_html(section_note(field.section)),
            ));
            html.push_str("<div class=\"fields\">\n");
            section = field.section;
            open = true;
        }
        html.push_str(&field_row(field));
    }
    if open {
        html.push_str("</div>\n</details>\n");
    }

    let disabled = if settings.writable { "" } else { " disabled" };
    html.push_str(&format!(
        "<div class=\"savebar\">\n\
<button class=\"primary\" id=\"save\" type=\"button\"{disabled}>Save changes</button>\n\
<button id=\"reset\" type=\"button\"{disabled}>Reset</button>\n\
<span class=\"status\" id=\"status\" role=\"status\" aria-live=\"polite\"></span>\n\
</div>\n</form>\n</section>\n"
    ));
    html
}

/// One row of the form: the label, the key, and the control for its kind.
fn field_row(field: &FieldValue) -> String {
    let key_name = field.key();
    let id = format!("f-{key_name}");
    let (control, hint) = field_control(field, &id);
    format!(
        "<div class=\"field\">\n\
<div class=\"meta\">\n<label for=\"{id}\">{label}</label>\n\
<code>{key}</code>{restart}\n</div>\n\
<div class=\"control\">{control}{hint}</div>\n\
</div>\n",
        label = escape_html(field.spec.label),
        key = escape_html(key_name),
        restart = if field.spec.restart_required {
            "<span class=\"tag\" title=\"saved now, applied on the next start\">restart</span>"
        } else {
            ""
        },
    )
}

/// The input a field's kind needs, plus the hint under it.
fn field_control(field: &FieldValue, id: &str) -> (String, String) {
    let key = field.key();
    match field.spec.kind {
        FieldKind::Bool => {
            let checked = if field.value.as_bool().unwrap_or(false) {
                " checked"
            } else {
                ""
            };
            (
                format!(
                    "<label class=\"check\"><input id=\"{id}\" type=\"checkbox\" \
data-key=\"{key}\" data-kind=\"bool\"{checked}><span>enabled</span></label>"
                ),
                String::new(),
            )
        }
        FieldKind::Integer | FieldKind::Float => {
            let step = if field.spec.kind == FieldKind::Integer {
                "1"
            } else {
                "any"
            };
            (
                format!(
                    "<input id=\"{id}\" type=\"number\" step=\"{step}\"{min}{max} \
data-key=\"{key}\" data-kind=\"number\" value=\"{value}\">",
                    min = number_bound("min", field.spec.min),
                    max = number_bound("max", field.spec.max),
                    value = format_number(&field.value),
                ),
                range_hint(field.spec),
            )
        }
        FieldKind::String if !field.spec.options.is_empty() => {
            let current = field.value.as_str().unwrap_or_default();
            let mut options = String::new();
            for option in field.spec.options {
                options.push_str(&format!(
                    "<option value=\"{value}\"{selected}>{label}</option>",
                    value = escape_html(option),
                    selected = if *option == current { " selected" } else { "" },
                    label = escape_html(option),
                ));
            }
            (
                format!(
                    "<select id=\"{id}\" data-key=\"{key}\" data-kind=\"string\">{options}</select>"
                ),
                String::new(),
            )
        }
        FieldKind::String => (
            format!(
                "<input id=\"{id}\" type=\"text\" data-key=\"{key}\" data-kind=\"string\" \
value=\"{value}\">",
                value = escape_html(field.value.as_str().unwrap_or_default()),
            ),
            String::new(),
        ),
        FieldKind::Multipliers => {
            let factors = multipliers_as_map(&field.value);
            let known = {
                let mut keys = vec![scoring::NO_MOD_KEY];
                keys.extend(scoring::MOD_SLOTS.iter().map(|slot| slot.key));
                keys.join(", ")
            };
            (
                format!(
                    "<textarea id=\"{id}\" rows=\"10\" spellcheck=\"false\" \
data-key=\"{key}\" data-kind=\"multipliers\">{value}</textarea>",
                    value = escape_html(&format_multipliers_pretty(&factors)),
                ),
                format!(
                    "<span class=\"hint\">A JSON object of acronym to factor, e.g. \
<code>{{\"EZ\": 1.8, \"NF\": 0.5}}</code>. Factors of different mods multiply, and a play with \
no mods uses <code>NM</code>. Valid keys: {known}</span>",
                    known = escape_html(&known),
                ),
            )
        }
    }
}

/// A `min`/`max` attribute, or nothing when the field has no bound.
fn number_bound(attribute: &str, value: Option<f64>) -> String {
    match value {
        Some(value) => format!(" {attribute}=\"{}\"", format_float(value)),
        None => String::new(),
    }
}

/// The hint under a numeric field, naming its declared range.
fn range_hint(spec: &FieldSpec) -> String {
    match (spec.min, spec.max) {
        (Some(min), Some(max)) => format!(
            "<span class=\"hint\">{} to {}</span>",
            format_float(min),
            format_float(max)
        ),
        _ => String::new(),
    }
}

/// A number as the input's `value`.
fn format_number(value: &toml::Value) -> String {
    match value {
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => format_float(*number),
        other => other.to_string(),
    }
}

/// A multiplier table as a plain map, for the editor and the writer.
fn multipliers_as_map(value: &toml::Value) -> HashMap<String, f64> {
    let mut map = HashMap::new();
    if let Some(table) = value.as_table() {
        for (key, factor) in table {
            map.insert(key.clone(), toml_float(factor));
        }
    }
    map
}

/// The discovered overlays, using the same cards `/overlays` renders.
fn overlays_html(view: &LandingView<'_>) -> String {
    let mut html = String::from("<section class=\"panel\">\n<div class=\"panel-head\">\n");
    html.push_str("<h2>Browser overlays</h2>\n");
    html.push_str(
        "<p class=\"hint\">Point an OBS <em>Browser</em> source at a card's URL. Drop-in tosu v2 \
         overlays work unmodified.</p>\n</div>\n",
    );
    match view.overlays {
        Some((overlays, root)) => html.push_str(&overlays::dashboard_cards(overlays, root)),
        None => html.push_str(
            "<p class=\"hint\">Browser overlays are off. Set \
             <code>server.enable_overlays = true</code> to serve the folders in your overlay \
             directory.</p>",
        ),
    }
    html.push_str("\n</section>\n");
    html
}

/// The system logs panel with live tail and historic file inspection.
fn logs_html(standalone: bool) -> String {
    let open_class = if standalone { " open" } else { "" };
    let popout_btn = if standalone {
        r#"<a href="/" class="ghost">← Back to Settings</a>"#
    } else {
        r#"<a href="/logs" target="_blank" class="ghost" onclick="event.stopPropagation()">Pop-out ↗</a>"#
    };
    let height_style = if standalone {
        "height: calc(100vh - 300px); min-height: 480px;"
    } else {
        "max-height: 440px;"
    };

    format!(
        r#"<section class="panel logs-panel">
<div class="logs-collapsible{open_class}" id="logs-collapsible">
<div class="panel-head logs-summary" id="logs-toggle-head">
  <div class="logs-summary-left">
    <span class="chevron" id="logs-chevron">▶</span>
    <h2>System Logs &amp; Live Tail</h2>
    <span class="badge live-badge" id="log-status-badge">● Live Tail</span>
  </div>
  <div class="logs-summary-right">
    <button type="button" class="ghost small" id="logs-toggle-btn">Toggle View</button>
    {popout_btn}
  </div>
</div>
<div class="logs-body" id="logs-panel-body">
  <div class="logs-toolbar">
    <div class="logs-toolbar-group">
      <label for="log-source-select" class="logs-label">Log File:</label>
      <select id="log-source-select" class="logs-select">
        <option value="live">🔴 Live Stream (Current)</option>
      </select>
    </div>
    <div class="logs-toolbar-group logs-levels" id="log-level-filters">
      <button type="button" class="btn-chip active" data-level="ALL">ALL</button>
      <button type="button" class="btn-chip chip-info" data-level="INFO">INFO</button>
      <button type="button" class="btn-chip chip-warn" data-level="WARN">WARN</button>
      <button type="button" class="btn-chip chip-error" data-level="ERROR">ERROR</button>
      <button type="button" class="btn-chip chip-debug" data-level="DEBUG">DEBUG</button>
    </div>
    <div class="logs-toolbar-group logs-search-group">
      <input type="text" id="log-search-input" class="logs-search" placeholder="Filter messages..." spellcheck="false">
    </div>
    <div class="logs-toolbar-group logs-actions">
      <label class="logs-autoscroll-label">
        <input type="checkbox" id="log-autoscroll" checked> Auto-scroll
      </label>
      <button type="button" id="log-copy-50" class="ghost small" title="Copy last 50 lines to clipboard">Copy 50</button>
      <button type="button" id="log-copy-all" class="ghost small" title="Copy all visible lines">Copy All</button>
      <button type="button" id="log-download-btn" class="ghost small" title="Download log file">Download</button>
      <button type="button" id="log-clear-btn" class="ghost small" title="Clear console view">Clear</button>
    </div>
  </div>
  <div class="logs-console-wrapper" style="{height_style}">
    <div id="log-console" class="logs-console" tabindex="0" role="region" aria-label="Log output">
      <div class="log-empty-msg">Connecting to live log stream...</div>
    </div>
  </div>
  <div class="logs-statusbar">
    <span id="log-count-info">0 lines displayed</span>
    <span id="log-file-info">Stream: /api/logs/tail</span>
  </div>
</div>
</div>
</section>
"#,
        open_class = open_class,
        popout_btn = popout_btn,
        height_style = height_style
    )
}

/// Links to everything the server serves, so the page is a usable index rather
/// than only a settings form.
fn links_html() -> String {
    let links: &[(&str, &str, bool)] = &[
        ("/json/v2", "full v2 payload", true),
        ("/json", "gosumemory-compatible payload", true),
        ("/json/sc", "StreamCompanion payload", true),
        (overlays::OVERLAYS_BASE, "overlay dashboard", false),
        ("/logs", "system logs & live tail", false),
        ("/health", "attachment status", true),
    ];
    let mut html = String::from(
        "<section class=\"panel\">\n<div class=\"panel-head\">\n<h2>Endpoints</h2>\n</div>\n<ul class=\"links\">\n",
    );
    for (path, description, is_json) in links {
        let inspect_btn = if *is_json {
            format!(
                r#" <button type="button" class="btn-inspect" data-endpoint="{path}" title="Inspect {path} in this page">⚡ Inspect</button>"#,
                path = escape_html(path)
            )
        } else {
            String::new()
        };
        let inspect_attr = if *is_json {
            format!(r#" data-inspect="{path}""#, path = escape_html(path))
        } else {
            String::new()
        };
        let raw_link = if *is_json {
            format!(
                r#"<a href="{path}" target="_blank" class="raw-link" title="Open raw in new tab">↗ raw</a> "#,
                path = escape_html(path)
            )
        } else {
            String::new()
        };
        html.push_str(&format!(
            "<li><span class=\"link-title\"><a href=\"{path}\"{inspect_attr}>{path}</a>{inspect_btn}</span><span>{raw_link}{description}</span></li>\n",
            path = escape_html(path),
            inspect_attr = inspect_attr,
            inspect_btn = inspect_btn,
            raw_link = raw_link,
            description = escape_html(description),
        ));
    }
    html.push_str(
        "</ul>\n<p class=\"hint\">Sockets: <code>/websocket/v2</code>, <code>/websocket/v2/delta</code>, <code>/websocket/v2/precise</code>, <code>/ws</code>, \
         <code>/tokens</code>, <code>/websocket/commands</code>.</p>\n</section>\n",
    );
    html
}

/// The in-page JSON API Inspector & Live Poller modal.
fn json_inspector_html() -> String {
    r#"<div id="json-inspector-modal" class="json-modal" style="display: none;" role="dialog" aria-modal="true" aria-labelledby="json-modal-title">
<div class="json-modal-backdrop" id="json-modal-backdrop"></div>
<div class="json-modal-dialog">
  <div class="json-modal-header">
    <div class="json-header-left">
      <span class="json-modal-icon">⚡</span>
      <h3 id="json-modal-title">JSON API Inspector</h3>
      <div class="json-endpoint-picker">
        <select id="json-endpoint-select" class="json-select" title="Select API Endpoint">
          <option value="/json/v2">/json/v2 (Full v2 payload)</option>
          <option value="/json/v2/precise">/json/v2/precise (Precise tourney stream)</option>
          <option value="/json">/json (gosumemory payload)</option>
          <option value="/json/sc">/json/sc (StreamCompanion payload)</option>
          <option value="/health">/health (Attachment &amp; state)</option>
          <option value="/api/settings">/api/settings (Config JSON)</option>
          <option value="/api/logs">/api/logs (Log events JSON)</option>
          <option value="custom">Custom Endpoint...</option>
        </select>
        <input type="text" id="json-endpoint-custom" class="json-custom-input" placeholder="/path" style="display: none;" spellcheck="false">
      </div>
    </div>
    <div class="json-header-right">
      <button type="button" id="json-close-btn" class="ghost small json-close-btn" title="Close inspector (Esc)">✕</button>
    </div>
  </div>

  <div class="json-modal-toolbar">
    <div class="json-toolbar-group json-poll-controls">
      <button type="button" id="json-poll-toggle" class="btn-chip chip-poll" title="Toggle automatic periodic requests">
        <span class="poll-indicator" id="json-poll-dot"></span>
        <span id="json-poll-label">Live Poll: OFF</span>
      </button>
      <div class="json-interval-wrapper">
        <label for="json-poll-rate" class="json-poll-label">Every</label>
        <input type="number" id="json-poll-rate" class="json-num-input" value="0.2" min="0.05" max="60" step="0.05" title="Polling interval in seconds (0.2s = 200ms)">
        <span class="json-unit">s</span>
      </div>
      <button type="button" id="json-fetch-btn" class="ghost small" title="Fetch fresh JSON now">↻ Fetch</button>
    </div>

    <div class="json-toolbar-group json-search-controls">
      <input type="text" id="json-search-input" class="json-search" placeholder="Filter keys/values... (or use Ctrl+F)" spellcheck="false">
      <span id="json-search-count" class="json-search-count"></span>
    </div>

    <div class="json-toolbar-group json-view-controls">
      <div class="json-tab-group" id="json-view-mode">
        <button type="button" class="btn-chip active" data-mode="tree">Tree</button>
        <button type="button" class="btn-chip" data-mode="raw">Raw</button>
      </div>
      <button type="button" id="json-expand-all" class="ghost small" title="Expand all nodes">Expand All</button>
      <button type="button" id="json-collapse-all" class="ghost small" title="Collapse all nodes">Collapse</button>
      <button type="button" id="json-copy-btn" class="ghost small" title="Copy JSON payload to clipboard">Copy</button>
    </div>
  </div>

  <div class="json-modal-body">
    <div id="json-tree-container" class="json-tree-view" tabindex="0">
      <div class="json-placeholder">Click an endpoint or Fetch to inspect JSON data.</div>
    </div>
    <pre id="json-raw-container" class="json-raw-view" style="display: none;" tabindex="0"><code id="json-raw-code"></code></pre>
  </div>

  <div class="json-modal-footer">
    <div class="json-footer-left">
      <span id="json-status-tag" class="badge">Idle</span>
      <span id="json-meta-info" class="hint">No requests yet</span>
    </div>
    <div class="json-footer-right">
      <span class="hint json-tip">Press <kbd>Ctrl+F</kbd> for browser search &bull; Nodes preserve expand/collapse on live polling</span>
    </div>
  </div>
</div>
</div>
"#.to_string()
}

/// The footer: where the code lives, and credit where the API design came from.
fn footer_html() -> String {
    format!(
        "<footer class=\"foot\"><div class=\"wrap foot-inner\">\n\
<div class=\"foot-project\">\n\
<span class=\"brand small\"><span class=\"mark\">R</span>rtosu<span class=\"dot\">&middot;</span>dataprovider</span>\n\
<p class=\"hint\">A standalone, memory-reading data server for osu! tournament overlays. \
Source: <a href=\"{github}\" target=\"_blank\" rel=\"noreferrer noopener\">github.com/Raregendary/rtosu-dataprovider</a></p>\n\
</div>\n\
<p class=\"hint credit\">Credit to the original <a href=\"{tosu}\" target=\"_blank\" rel=\"noreferrer noopener\"><strong>tosu</strong></a> \
project, whose data API and overlay format this server reproduces so that tosu overlays work unmodified.</p>\n\
</div></footer>\n",
        github = GITHUB_URL,
        tosu = TOSU_URL,
    )
}

/// The landing page's own stylesheet.
///
/// Deliberately self-contained -- no font, image or script is fetched from
/// anywhere but this server -- and written by hand rather than pulled from a
/// framework, because the whole page is one request served from a tournament
/// machine that may have no internet at all.
const LANDING_STYLE: &str = r#":root {
  color-scheme: dark;
  --bg: #0b0d12;
  --panel: #141824;
  --panel-2: #1a1f2d;
  --line: #232a3a;
  --line-2: #333d55;
  --text: #e9ecf3;
  --muted: #93a0b8;
  --faint: #6d7891;
  --pink: #ff74ad;
  --blue: #7aa7ff;
  --ok: #62d492;
  --warn: #ffc46b;
  --err: #ff8b8b;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  font: 14px/1.55 "Segoe UI", system-ui, -apple-system, sans-serif;
  color: var(--text);
  background:
    radial-gradient(1200px 560px at 15% -10%, #1d2742 0%, rgba(29, 39, 66, 0) 60%),
    radial-gradient(1000px 520px at 100% -5%, #2b1b2e 0%, rgba(43, 27, 46, 0) 55%),
    var(--bg);
  background-attachment: fixed;
}
a { color: var(--blue); text-decoration: none; }
a:hover { text-decoration: underline; }
code { font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; }

.top {
  position: sticky; top: 0; z-index: 10;
  background: rgba(11, 13, 18, .85);
  backdrop-filter: blur(10px);
  border-bottom: 1px solid var(--line);
}
.top-inner {
  max-width: 1160px; margin: 0 auto; padding: 12px 26px;
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
}
.brand { display: inline-flex; align-items: center; font-size: 16px; font-weight: 650; letter-spacing: .2px; }
.brand .mark {
  display: inline-grid; place-items: center; width: 26px; height: 26px; margin-right: 9px;
  border-radius: 8px; background: linear-gradient(135deg, var(--pink), #7c5cff);
  color: #0b0d12; font-weight: 800; font-size: 14px;
}
.brand.small { font-size: 14px; }
.brand .dot { color: var(--pink); margin: 0 3px; }
.pill {
  max-width: 100%; padding: 3px 10px; font-size: 12px; color: var(--muted);
  background: var(--panel-2); border: 1px solid var(--line); border-radius: 999px;
  overflow-wrap: anywhere;
}
a.pill:hover { border-color: var(--line-2); color: var(--text); text-decoration: none; }
.top-actions { margin-left: auto; display: flex; align-items: center; gap: 8px; }
.ghost {
  padding: 6px 13px; font: inherit; font-size: 12.5px; color: var(--text);
  background: var(--panel-2); border: 1px solid var(--line-2); border-radius: 8px;
  cursor: pointer; text-decoration: none; white-space: nowrap;
}
.ghost:hover:not(:disabled) { background: #242b3c; border-color: #43506f; text-decoration: none; }
.ghost.danger { color: #ffd7e7; border-color: #5d2a44; background: #241320; }
.ghost.danger:hover:not(:disabled) { background: #33192b; border-color: #7c3a5a; }
.ghost:disabled { opacity: .55; cursor: progress; }

.banner {
  margin: 16px 0 0; padding: 11px 14px; font-size: 13px;
  border: 1px solid var(--line-2); border-radius: 10px;
  background: var(--panel); color: var(--muted);
}
.banner.warn { border-color: #5b4620; background: #241d10; color: var(--warn); }
.banner.err { border-color: #5d2a2a; background: #251314; color: var(--err); }
.banner.ok { border-color: #2c4a39; background: #10241a; color: var(--ok); }
.banner[hidden] { display: none; }

.wrap { max-width: 1160px; margin: 0 auto; padding: 0 26px 40px; }
.wrap.tight { padding-bottom: 0; }
main.wrap {
  padding-top: 22px;
  display: flex; flex-direction: column; gap: 22px;
}
.panel {
  padding: 20px; border: 1px solid var(--line); border-radius: 14px;
  background: linear-gradient(180deg, rgba(26, 31, 45, .72), rgba(20, 24, 36, .78));
  box-shadow: 0 10px 30px rgba(0, 0, 0, .25);
}
.panel-head { margin-bottom: 14px; }
.panel h2 {
  display: flex; align-items: center; gap: 8px;
  margin: 0 0 4px; font-size: 13px; font-weight: 650; letter-spacing: .8px;
  text-transform: uppercase; color: var(--muted);
}
.panel h2::before {
  content: ""; flex: none; width: 8px; height: 8px; border-radius: 3px;
  background: linear-gradient(135deg, var(--pink), #7c5cff);
}
p { margin: 0; }
.hint { font-size: 12px; color: var(--faint); }
.hint code { color: var(--pink); }

details.section {
  margin: 0 0 14px; border: 1px solid var(--line); border-radius: 12px;
  background: var(--panel-2); overflow: hidden;
}
details.section:last-of-type { margin-bottom: 0; }
details.section > summary {
  display: flex; align-items: center; gap: 10px; padding: 12px 15px;
  cursor: pointer; list-style: none; user-select: none;
}
details.section > summary::-webkit-details-marker { display: none; }
details.section > summary:hover { background: rgba(122, 167, 255, .05); }
details[open] > summary { border-bottom: 1px solid var(--line); }
.chev {
  flex: none; width: 0; height: 0;
  border-left: 6px solid var(--faint);
  border-top: 5px solid transparent; border-bottom: 5px solid transparent;
  transition: transform .15s ease;
}
details[open] > summary .chev { transform: rotate(90deg); }
summary .s-title { font-size: 14px; font-weight: 600; }
summary .key { font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; font-size: 11.5px; color: var(--faint); }
summary .s-note {
  flex: 1 1 auto; min-width: 0; text-align: right;
  font-size: 12px; color: var(--faint);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.fields { display: flex; flex-direction: column; padding: 2px 15px 8px; }
.field {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(220px, 340px);
  gap: 6px 18px; align-items: center;
  padding: 10px 6px; border-top: 1px solid rgba(35, 42, 58, .9);
  border-radius: 8px;
}
.field:hover { background: rgba(122, 167, 255, .04); }
.field:first-child { border-top: 0; }
.meta { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
.meta label { font-size: 13px; }
.meta code { font-size: 11.5px; color: var(--faint); }
.tag {
  padding: 1px 6px; font-size: 10.5px; letter-spacing: .4px; text-transform: uppercase;
  color: var(--warn); border: 1px solid #5b4620; border-radius: 5px;
}
.control { display: flex; flex-direction: column; gap: 4px; }
.control input[type="text"], .control input[type="number"], .control select, .control textarea {
  width: 100%; padding: 7px 9px; font: inherit; font-size: 13px;
  color: var(--text); background: #0f1219;
  border: 1px solid var(--line-2); border-radius: 8px;
}
.control textarea {
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
  font-size: 12px; line-height: 1.45; resize: vertical;
}
.control input:focus, .control select:focus, .control textarea:focus {
  outline: 2px solid rgba(122, 167, 255, .45); outline-offset: 1px; border-color: #3d5fa8;
}
.control input:disabled, .control select:disabled, .control textarea:disabled {
  opacity: .55; cursor: not-allowed;
}
.check { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; color: var(--muted); cursor: pointer; }
input[type="checkbox"] { width: 16px; height: 16px; accent-color: var(--pink); cursor: pointer; }

.savebar {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  margin-top: 18px; padding-top: 16px; border-top: 1px solid var(--line);
}
.savebar button {
  padding: 8px 16px; font: inherit; font-size: 13px; color: var(--text);
  background: var(--panel-2); border: 1px solid var(--line-2); border-radius: 8px; cursor: pointer;
}
.savebar button:hover:not(:disabled) { background: #232936; }
.savebar button.primary {
  color: #1a0d14; background: var(--pink); border-color: var(--pink); font-weight: 600;
}
.savebar button.primary:hover:not(:disabled) { background: #ff8dbc; }
.savebar button:disabled { opacity: .5; cursor: not-allowed; }
.status { font-size: 12.5px; color: var(--faint); }
.status.ok { color: var(--ok); }
.status.err { color: var(--err); }
.status.busy { color: var(--muted); }

ul.links {
  display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 10px; list-style: none; margin: 0 0 12px; padding: 0;
}
ul.links li {
  display: flex; flex-direction: column; gap: 2px;
  padding: 9px 12px; border: 1px solid var(--line); border-radius: 9px;
  background: rgba(13, 16, 24, .55);
}
ul.links a { font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; font-size: 12.5px; }
ul.links span { font-size: 11.5px; color: var(--faint); }

.foot { border-top: 1px solid var(--line); background: rgba(11, 13, 18, .6); }
.foot-inner {
  display: flex; align-items: flex-end; justify-content: space-between;
  gap: 14px 26px; flex-wrap: wrap; padding: 22px 26px 30px;
}
.foot-project .hint { margin-top: 6px; }
.credit { max-width: 56ch; }
.credit strong { color: var(--pink); font-weight: 650; }

/* Logs Panel and Console */
.logs-panel { margin-bottom: 24px; }
.logs-collapsible #logs-toggle-head { cursor: pointer; user-select: none; }
.logs-collapsible .chevron { display: inline-block; font-size: 11px; color: var(--muted); transition: transform .2s ease; }
.logs-collapsible.open .chevron { transform: rotate(90deg); }
.logs-collapsible:not(.open) .logs-body { display: none; }

.badge.live-badge {
  font-size: 11.5px; font-weight: 600; padding: 3px 9px; border-radius: 999px;
  background: rgba(98, 212, 146, 0.12); color: var(--ok); border: 1px solid rgba(98, 212, 146, 0.25);
  display: inline-flex; align-items: center; gap: 5px;
}
.badge.historic-badge {
  font-size: 11.5px; font-weight: 600; padding: 3px 9px; border-radius: 999px;
  background: rgba(122, 167, 255, 0.12); color: var(--blue); border: 1px solid rgba(122, 167, 255, 0.25);
}
.badge.err-badge {
  font-size: 11.5px; font-weight: 600; padding: 3px 9px; border-radius: 999px;
  background: rgba(255, 139, 139, 0.12); color: var(--err); border: 1px solid rgba(255, 139, 139, 0.25);
}

.logs-body { margin-top: 14px; display: flex; flex-direction: column; gap: 10px; }
.logs-toolbar {
  display: flex; align-items: center; justify-content: space-between; gap: 10px; flex-wrap: wrap;
  padding: 8px 12px; background: rgba(13, 16, 24, .65); border: 1px solid var(--line); border-radius: 8px;
}
.logs-toolbar-group { display: flex; align-items: center; gap: 6px; }
.logs-label { font-size: 12px; color: var(--muted); font-weight: 600; }
.logs-select {
  padding: 4px 8px; font-size: 12.5px; color: var(--text); background: var(--panel-2);
  border: 1px solid var(--line-2); border-radius: 6px; cursor: pointer; outline: none;
}
.logs-select:focus { border-color: var(--blue); }

.btn-chip {
  padding: 3px 9px; font-size: 11px; font-weight: 650; color: var(--muted);
  background: var(--panel); border: 1px solid var(--line); border-radius: 6px;
  cursor: pointer; transition: all .15s ease;
}
.btn-chip:hover { border-color: var(--line-2); color: var(--text); }
.btn-chip.active { background: var(--panel-2); color: var(--text); border-color: var(--blue); }
.btn-chip.chip-info.active { color: #62d492; border-color: #62d492; }
.btn-chip.chip-warn.active { color: #ffc46b; border-color: #ffc46b; }
.btn-chip.chip-error.active { color: #ff8b8b; border-color: #ff8b8b; }
.btn-chip.chip-debug.active { color: #c48eff; border-color: #c48eff; }

.logs-search {
  padding: 4px 10px; font-size: 12px; color: var(--text); background: var(--panel-2);
  border: 1px solid var(--line); border-radius: 6px; min-width: 170px; outline: none;
}
.logs-search:focus { border-color: var(--blue); }
.logs-autoscroll-label { font-size: 12px; color: var(--muted); display: inline-flex; align-items: center; gap: 4px; cursor: pointer; margin-right: 4px; }

.ghost.small { padding: 4px 9px; font-size: 11.5px; }

.logs-console-wrapper {
  display: flex; flex-direction: column; overflow: hidden;
  border: 1px solid var(--line); border-radius: 8px; background: #07090e;
}
.logs-console {
  flex: 1 1 auto; overflow-y: auto; overflow-x: auto; padding: 12px 14px;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace; font-size: 12px; line-height: 1.6;
  white-space: pre-wrap; word-break: break-word; color: #d0d7de;
}
.logs-console:focus-visible { outline: 1px solid var(--blue); }

.log-entry-row {
  display: flex; gap: 8px; padding: 1px 4px; border-radius: 4px;
}
.log-entry-row:hover { background: rgba(255, 255, 255, .04); }
.log-col-time { color: var(--faint); flex-shrink: 0; font-size: 11px; }
.log-col-level {
  font-size: 10px; font-weight: 750; letter-spacing: .3px; padding: 0 4px;
  border-radius: 3px; flex-shrink: 0; height: 18px; line-height: 18px; text-align: center;
}
.log-lvl-info { color: #62d492; background: rgba(98, 212, 146, 0.12); }
.log-lvl-warn { color: #ffc46b; background: rgba(255, 196, 107, 0.12); }
.log-lvl-error { color: #ff8b8b; background: rgba(255, 139, 139, 0.15); }
.log-lvl-debug, .log-lvl-trace { color: #c48eff; background: rgba(196, 142, 255, 0.12); }

.log-col-target { color: var(--blue); flex-shrink: 0; font-size: 11.5px; opacity: .85; }
.log-col-msg { color: var(--text); flex: 1 1 auto; }
.log-empty-msg { color: var(--faint); font-style: italic; padding: 16px; text-align: center; }

.logs-statusbar {
  display: flex; align-items: center; justify-content: space-between;
  padding: 4px 8px; font-size: 11.5px; color: var(--faint);
}
.logs-standalone-wrap { max-width: 1400px; }

@media (max-width: 720px) {
  .field { grid-template-columns: minmax(0, 1fr); }
  summary .s-note { display: none; }
  .top-actions { margin-left: 0; width: 100%; }
}

/* ---- Endpoints & Inspect Buttons ---- */
.link-title { display: inline-flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.btn-inspect {
  background: rgba(122, 167, 255, 0.15); color: var(--blue);
  border: 1px solid rgba(122, 167, 255, 0.35); border-radius: 4px;
  padding: 1px 7px; font-size: 11px; font-weight: 600; cursor: pointer;
  line-height: 1.4; transition: all 0.15s ease;
}
.btn-inspect:hover {
  background: rgba(122, 167, 255, 0.3); border-color: var(--blue);
  color: #fff;
}
.raw-link {
  color: var(--faint); font-size: 11px; margin-right: 6px;
  text-decoration: none;
}
.raw-link:hover { color: var(--blue); text-decoration: underline; }

/* ---- JSON API Inspector Modal ---- */
.json-modal {
  position: fixed; inset: 0; z-index: 2000;
  display: flex; align-items: center; justify-content: center;
}
.json-modal-backdrop {
  position: absolute; inset: 0; background: rgba(8, 10, 15, 0.84);
  backdrop-filter: blur(6px);
}
.json-modal-dialog {
  position: relative; z-index: 1;
  width: min(1200px, 95vw); height: min(880px, 88vh);
  background: var(--panel); border: 1px solid var(--line-2);
  border-radius: 8px; box-shadow: 0 20px 60px rgba(0,0,0,0.65);
  display: flex; flex-direction: column; overflow: hidden;
}
.json-modal-header {
  display: flex; justify-content: space-between; align-items: center;
  padding: 10px 16px; border-bottom: 1px solid var(--line);
  background: var(--panel-2); gap: 12px;
}
.json-header-left {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap; flex: 1 1 auto;
}
.json-modal-icon { font-size: 18px; color: var(--warn); }
.json-modal-header h3 {
  margin: 0; font-size: 15px; font-weight: 700; color: var(--text);
  white-space: nowrap;
}
.json-endpoint-picker { display: flex; align-items: center; gap: 6px; }
.json-select {
  background: #0f131c; color: var(--text); border: 1px solid var(--line-2);
  border-radius: 4px; padding: 4px 8px; font-size: 12.5px;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
}
.json-custom-input {
  background: #0f131c; color: var(--text); border: 1px solid var(--line-2);
  border-radius: 4px; padding: 4px 8px; font-size: 12.5px; width: 140px;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
}
.json-close-btn { font-size: 16px; padding: 2px 8px; border-radius: 4px; }

/* ---- JSON Toolbar ---- */
.json-modal-toolbar {
  display: flex; flex-wrap: wrap; gap: 10px; align-items: center;
  justify-content: space-between; padding: 8px 16px;
  border-bottom: 1px solid var(--line); background: rgba(0,0,0,0.22);
}
.json-toolbar-group { display: flex; align-items: center; gap: 6px; }
.chip-poll {
  display: inline-flex; align-items: center; gap: 6px; font-weight: 600;
}
.chip-poll.active {
  background: rgba(98, 212, 146, 0.18); border-color: rgba(98, 212, 146, 0.5);
  color: #62d492;
}
.poll-indicator {
  display: inline-block; width: 8px; height: 8px; border-radius: 50%;
  background: var(--faint);
}
.chip-poll.active .poll-indicator {
  background: #62d492;
  box-shadow: 0 0 6px #62d492;
  animation: pulse-dot 1.2s infinite;
}
@keyframes pulse-dot {
  0% { transform: scale(0.95); opacity: 0.8; }
  50% { transform: scale(1.25); opacity: 1; }
  100% { transform: scale(0.95); opacity: 0.8; }
}
.json-interval-wrapper {
  display: inline-flex; align-items: center; gap: 4px;
  background: #0f131c; border: 1px solid var(--line-2); border-radius: 4px;
  padding: 2px 6px; font-size: 12px; color: var(--muted);
}
.json-poll-label { font-size: 11px; }
.json-num-input {
  background: transparent; border: none; color: var(--text);
  width: 44px; font-size: 12px; text-align: center;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
}
.json-num-input:focus { outline: none; }
.json-unit { font-size: 11px; color: var(--faint); }
.json-search {
  background: #0f131c; color: var(--text); border: 1px solid var(--line-2);
  border-radius: 4px; padding: 4px 8px; font-size: 12px; width: 220px;
}
.json-search:focus { outline: 1px solid var(--blue); }
.json-search-count { font-size: 11px; color: var(--warn); min-width: 40px; }
.json-tab-group { display: flex; gap: 2px; }

/* ---- JSON Modal Body ---- */
.json-modal-body {
  flex: 1 1 auto; overflow: hidden; position: relative; display: flex;
}
.json-tree-view {
  flex: 1 1 auto; overflow-y: auto; overflow-x: auto; padding: 14px 18px;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
  font-size: 12.5px; line-height: 1.6; color: #d0d7de;
}
.json-raw-view {
  flex: 1 1 auto; overflow: auto; padding: 14px 18px; margin: 0;
  font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
  font-size: 12px; line-height: 1.6; color: #d0d7de; background: #080a0f;
}
.json-placeholder {
  color: var(--faint); font-style: italic; padding: 24px; text-align: center;
}

/* ---- Tree Node Hierarchy & Styling ---- */
.json-node {
  position: relative; padding-left: 18px; white-space: pre-wrap; word-break: break-all;
}
.json-caret {
  position: absolute; left: 0; top: 1px; width: 14px; height: 16px;
  cursor: pointer; user-select: none; display: inline-flex; align-items: center;
  justify-content: center; font-size: 9px; color: var(--faint);
  transition: transform 0.15s ease;
}
.json-caret:hover { color: var(--blue); }
.json-caret.collapsed { transform: rotate(-90deg); }
.json-key {
  color: #79c0ff; font-weight: 600; cursor: pointer;
}
.json-key:hover { text-decoration: underline; }
.json-val.json-string { color: #a5d6ff; }
.json-val.json-number { color: #79c0ff; font-weight: 500; }
.json-val.json-bool { color: #ff7b72; font-weight: 600; }
.json-val.json-null { color: #8b949e; font-style: italic; }
.json-bracket { color: #8b949e; }
.json-comma { color: #8b949e; }
.json-item-count {
  color: var(--faint); font-size: 11px; margin-left: 6px; font-style: italic;
  user-select: none;
}
.json-children.collapsed { display: none; }
.json-children.collapsed[hidden="until-found"] { display: none; }
.json-search-match {
  background: rgba(255, 214, 102, 0.35); border-radius: 2px;
  outline: 1px solid rgba(255, 214, 102, 0.6);
}
.json-val-flash {
  background: rgba(98, 212, 146, 0.25); border-radius: 2px;
  transition: background 0.4s ease;
}

/* ---- JSON Modal Footer ---- */
.json-modal-footer {
  display: flex; justify-content: space-between; align-items: center;
  padding: 6px 16px; border-top: 1px solid var(--line);
  background: var(--panel-2); font-size: 11.5px;
}
.json-footer-left { display: flex; align-items: center; gap: 8px; }
.json-footer-right { display: flex; align-items: center; gap: 8px; }
.json-tip kbd {
  background: rgba(255,255,255,0.08); border: 1px solid var(--line-2);
  border-radius: 3px; padding: 1px 4px; font-size: 10px; font-family: inherit;
}
"#;

/// The landing page's script.
///
/// No framework and no transpile step: the whole thing exists to send the
/// changed controls to `POST /api/settings` and to say what happened.
const LANDING_SCRIPT: &str = r#"(function () {
  var meta = {};
  try {
    meta = JSON.parse(document.getElementById('rtosu-data').textContent) || {};
  } catch (error) {
    meta = {};
  }

  var controls = Array.prototype.slice.call(document.querySelectorAll('[data-key]'));
  var status = document.getElementById('status');
  var banner = document.getElementById('banner');
  var save = document.getElementById('save');
  var reset = document.getElementById('reset');
  var baseline = {};
  var originals = {};

  // The control's value, as the JSON the API expects. A textarea that does not
  // parse is reported instead of being sent, so the server never has to explain
  // a JSON syntax error back to the page.
  function read(control) {
    if (control.type === 'checkbox') {
      return { ok: true, value: control.checked };
    }
    if (control.tagName === 'TEXTAREA') {
      try {
        var parsed = JSON.parse(control.value);
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
          return { ok: false, error: 'must be a JSON object' };
        }
        return { ok: true, value: parsed };
      } catch (error) {
        return { ok: false, error: 'is not valid JSON (' + error.message + ')' };
      }
    }
    if (control.type === 'number') {
      var number = Number(control.value);
      if (control.value.trim() === '' || !isFinite(number)) {
        return { ok: false, error: 'must be a number' };
      }
      return { ok: true, value: number };
    }
    return { ok: true, value: control.value };
  }

  function same(left, right) {
    return JSON.stringify(left) === JSON.stringify(right);
  }

  function setStatus(text, kind) {
    if (!status) return;
    status.textContent = text || '';
    status.className = kind ? 'status ' + kind : 'status';
  }

  function remember(control) {
    originals[control.dataset.key] = control.value;
    var current = read(control);
    baseline[control.dataset.key] = current.ok ? current.value : control.value;
  }

  controls.forEach(remember);

  if (save) {
    save.addEventListener('click', function () {
      if (!meta.writable) return;

      var patch = {};
      var invalid = null;
      controls.forEach(function (control) {
        var key = control.dataset.key;
        var current = read(control);
        if (!current.ok) {
          if (!invalid) invalid = key + ' ' + current.error;
          return;
        }
        if (!same(current.value, baseline[key])) patch[key] = current.value;
      });

      if (invalid) {
        setStatus(invalid, 'err');
        return;
      }
      if (!Object.keys(patch).length) {
        setStatus('Nothing changed.', '');
        return;
      }

      setStatus('Saving...', 'busy');
      save.disabled = true;
      fetch('/api/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(patch)
      }).then(function (response) {
        return response.text().then(function (text) {
          var body = null;
          try { body = JSON.parse(text); } catch (error) { body = null; }
          return { status: response.status, body: body };
        });
      }).then(function (result) {
        save.disabled = false;
        if (result.status === 200 && result.body && result.body.ok) {
          controls.forEach(remember);
          var count = (result.body.applied || []).length;
          setStatus('Saved ' + count + ' setting' + (count === 1 ? '' : 's') + '.', 'ok');
          var restarting = result.body.restart_required || [];
          if (restarting.length && banner) {
            banner.textContent = 'Saved. A restart is needed before these take effect: ' +
              restarting.join(', ') + '.';
            banner.className = 'banner warn';
            banner.hidden = false;
          }
        } else if (result.body && result.body.error) {
          setStatus(result.body.error, 'err');
        } else {
          setStatus('Save failed: HTTP ' + result.status, 'err');
        }
      }).catch(function (error) {
        save.disabled = false;
        setStatus('Save failed: ' + error, 'err');
      });
    });
  }

  if (reset) {
    reset.addEventListener('click', function () {
      controls.forEach(function (control) {
        if (control.type === 'checkbox') {
          control.checked = baseline[control.dataset.key];
        } else if (control.value !== originals[control.dataset.key]) {
          control.value = originals[control.dataset.key];
        }
      });
      setStatus('Reverted to the values the server last confirmed.', '');
    });
  }

  // The overlay cards' copy button, the same handler /overlays uses.
  document.addEventListener('click', function (event) {
    var button = event.target.closest('button[data-copy]');
    if (!button) return;
    var value = button.getAttribute('data-copy');
    var done = function () {
      var previous = button.textContent;
      button.textContent = 'Copied';
      setTimeout(function () { button.textContent = previous; }, 1200);
    };
    if (navigator.clipboard) {
      navigator.clipboard.writeText(value).then(done, done);
    } else {
      done();
    }
  });

  // Section collapse memory: an operator who hides [logging] keeps it hidden
  // across reloads, including the reload a restart triggers.
  Array.prototype.forEach.call(document.querySelectorAll('details.section'), function (section) {
    var key = 'rtosu.section.' + section.id;
    var saved = null;
    try { saved = window.localStorage.getItem(key); } catch (error) {}
    if (saved === '1' || saved === '0') section.open = saved === '1';
    section.addEventListener('toggle', function () {
      try { window.localStorage.setItem(key, section.open ? '1' : '0'); } catch (error) {}
    });
  });

  // Hot restart. The button is rendered only when the endpoint would honour
  // this browser: a CLI `serve` process with a supervisor, viewed from the
  // machine itself. After the request lands, poll until the replacement
  // process answers, then hand the browser over to it.
  var restart = document.getElementById('restart');
  if (restart) {
    restart.addEventListener('click', function () {
      if (!window.confirm('Restart the server now? Live overlay sockets drop for a moment while a new process takes over this port.')) return;
      restart.disabled = true;
      setStatus('Restart requested...', 'busy');
      fetch('/api/restart', { method: 'POST' }).then(function (response) {
        return response.text().then(function (text) {
          var body = null;
          try { body = JSON.parse(text); } catch (error) { body = null; }
          return { status: response.status, body: body };
        });
      }).then(function (result) {
        if (result.status === 200 && result.body && result.body.ok) {
          restart.textContent = 'Restarting...';
          setStatus('Restarting. This page reloads when the new process is up.', 'ok');
          if (banner) {
            banner.textContent = 'Restarting the server: a new process is taking over this port. This page reloads as soon as it answers.';
            banner.className = 'banner warn';
            banner.hidden = false;
          }
          var wait = function (attempt) {
            setTimeout(function () {
              fetch('/', { method: 'GET', cache: 'no-store' }).then(function (probe) {
                if (probe.ok) { location.replace('/'); }
                else if (attempt < 20) { wait(attempt + 1); }
              }).catch(function () {
                if (attempt < 20) {
                  wait(attempt + 1);
                } else {
                  setStatus('The new process is not answering yet. Reload this page manually in a moment.', 'err');
                  restart.disabled = false;
                }
              });
            }, 1000);
          };
          wait(0);
        } else {
          restart.disabled = false;
          setStatus((result.body && result.body.error) || ('Restart failed: HTTP ' + result.status), 'err');
        }
      }).catch(function (error) {
        restart.disabled = false;
        setStatus('Restart failed: ' + error, 'err');
      });
    });
  }

  // ---- System Logs & Live Tail Controller ----
  (function initLogs() {
    var consoleElem = document.getElementById('log-console');
    if (!consoleElem) return;

    var sourceSelect = document.getElementById('log-source-select');
    var searchInput = document.getElementById('log-search-input');
    var autoscrollCheck = document.getElementById('log-autoscroll');
    var badge = document.getElementById('log-status-badge');
    var countInfo = document.getElementById('log-count-info');
    var fileInfo = document.getElementById('log-file-info');
    var levelFilters = document.getElementById('log-level-filters');
    var copy50Btn = document.getElementById('log-copy-50');
    var copyAllBtn = document.getElementById('log-copy-all');
    var downloadBtn = document.getElementById('log-download-btn');
    var clearBtn = document.getElementById('log-clear-btn');

    var currentEntries = [];
    var activeLevel = 'ALL';
    var activeSearch = '';
    var eventSource = null;
    var userScrolledUp = false;
    var collapsible = document.getElementById('logs-collapsible');
    var toggleHead = document.getElementById('logs-toggle-head');
    var toggleBtn = document.getElementById('logs-toggle-btn');
    var toggleCollapse = function (e) {
      if (e.target.closest('a') || e.target.closest('select') || e.target.closest('input') || e.target.closest('button:not(#logs-toggle-btn)')) return;
      if (collapsible) {
        collapsible.classList.toggle('open');
      }
    };
    if (toggleHead) toggleHead.addEventListener('click', toggleCollapse);
    if (toggleBtn) toggleBtn.addEventListener('click', function (e) {
      e.stopPropagation();
      if (collapsible) collapsible.classList.toggle('open');
    });

    consoleElem.addEventListener('scroll', function () {
      var atBottom = consoleElem.scrollHeight - consoleElem.scrollTop - consoleElem.clientHeight < 30;
      userScrolledUp = !atBottom;
    });

    function scrollToBottom() {
      if (autoscrollCheck && autoscrollCheck.checked && !userScrolledUp) {
        consoleElem.scrollTop = consoleElem.scrollHeight;
      }
    }

    function renderRow(entry) {
      var row = document.createElement('div');
      row.className = 'log-entry-row';
      row.dataset.level = (entry.level || 'INFO').toUpperCase();

      var timeCol = document.createElement('span');
      timeCol.className = 'log-col-time';
      timeCol.textContent = entry.timestamp ? (entry.timestamp.split('T')[1] || entry.timestamp) : '';

      var lvlCol = document.createElement('span');
      var lvlUpper = (entry.level || 'INFO').toUpperCase();
      lvlCol.className = 'log-col-level log-lvl-' + lvlUpper.toLowerCase();
      lvlCol.textContent = lvlUpper;

      var tgtCol = document.createElement('span');
      tgtCol.className = 'log-col-target';
      tgtCol.textContent = entry.target ? entry.target + ':' : '';

      var msgCol = document.createElement('span');
      msgCol.className = 'log-col-msg';
      msgCol.textContent = entry.message || entry.raw || '';

      row.appendChild(timeCol);
      row.appendChild(lvlCol);
      if (entry.target) row.appendChild(tgtCol);
      row.appendChild(msgCol);

      return row;
    }

    function matchesFilter(entry) {
      if (activeLevel !== 'ALL') {
        var lvl = (entry.level || 'INFO').toUpperCase();
        if (lvl !== activeLevel) return false;
      }
      if (activeSearch) {
        var q = activeSearch.toLowerCase();
        var full = (entry.raw || ((entry.target || '') + ' ' + (entry.message || ''))).toLowerCase();
        if (full.indexOf(q) === -1) return false;
      }
      return true;
    }

    function refreshConsoleView() {
      consoleElem.innerHTML = '';
      var matchedCount = 0;
      var frag = document.createDocumentFragment();

      for (var i = 0; i < currentEntries.length; i++) {
        var e = currentEntries[i];
        if (matchesFilter(e)) {
          frag.appendChild(renderRow(e));
          matchedCount++;
        }
      }

      if (matchedCount === 0) {
        var empty = document.createElement('div');
        empty.className = 'log-empty-msg';
        empty.textContent = currentEntries.length === 0 ? 'No log entries available.' : 'No log entries match the current filter.';
        consoleElem.appendChild(empty);
      } else {
        consoleElem.appendChild(frag);
      }

      if (countInfo) {
        countInfo.textContent = matchedCount + ' of ' + currentEntries.length + ' lines';
      }
      scrollToBottom();
    }

    function appendEntry(entry) {
      currentEntries.push(entry);
      if (currentEntries.length > 2000) {
        currentEntries.shift();
      }

      if (matchesFilter(entry)) {
        var emptyMsg = consoleElem.querySelector('.log-empty-msg');
        if (emptyMsg) consoleElem.removeChild(emptyMsg);

        consoleElem.appendChild(renderRow(entry));
        if (countInfo) {
          countInfo.textContent = currentEntries.length + ' lines';
        }
        scrollToBottom();
      }
    }

    function connectLiveStream() {
      if (eventSource) {
        eventSource.close();
        eventSource = null;
      }

      currentEntries = [];
      refreshConsoleView();
      if (badge) {
        badge.className = 'badge live-badge';
        badge.textContent = '● Connecting...';
      }
      if (fileInfo) fileInfo.textContent = 'Stream: /api/logs/tail';

      eventSource = new EventSource('/api/logs/tail');

      eventSource.addEventListener('init', function (ev) {
        try {
          var entry = JSON.parse(ev.data);
          appendEntry(entry);
        } catch (e) {}
      });

      eventSource.addEventListener('log', function (ev) {
        try {
          var entry = JSON.parse(ev.data);
          appendEntry(entry);
        } catch (e) {}
      });

      eventSource.onopen = function () {
        if (badge) {
          badge.className = 'badge live-badge';
          badge.textContent = '● Live Tail';
        }
        refreshConsoleView();
      };

      eventSource.onerror = function () {
        if (badge) {
          badge.className = 'badge err-badge';
          badge.textContent = 'Disconnected (Reconnecting...)';
        }
      };
    }

    function loadHistoricFile(filename) {
      if (eventSource) {
        eventSource.close();
        eventSource = null;
      }

      if (badge) {
        badge.className = 'badge historic-badge';
        badge.textContent = 'Historic: ' + filename;
      }
      if (fileInfo) fileInfo.textContent = 'File: logs/' + filename;

      consoleElem.innerHTML = '<div class="log-empty-msg">Loading ' + filename + '...</div>';

      fetch('/api/logs/view?file=' + encodeURIComponent(filename) + '&lines=1000')
        .then(function (res) { return res.json(); })
        .then(function (data) {
          currentEntries = [];
          if (data && data.lines) {
            data.lines.forEach(function (line) {
              var tokens = line.trim().split(/\s+/);
              var ts = tokens[0] || '';
              var lvl = tokens[1] || 'INFO';
              var rem = line.slice(line.indexOf(lvl) + lvl.length).trim();
              var colon = rem.indexOf(':');
              var tgt = colon !== -1 ? rem.slice(0, colon).trim() : '';
              var msg = colon !== -1 ? rem.slice(colon + 1).trim() : rem;

              currentEntries.push({
                timestamp: ts,
                level: lvl,
                target: tgt,
                message: msg,
                raw: line
              });
            });
          }
          userScrolledUp = false;
          refreshConsoleView();
        })
        .catch(function (err) {
          consoleElem.innerHTML = '<div class="log-empty-msg">Failed to load log file: ' + err + '</div>';
        });
    }

    function fetchLogFileList() {
      fetch('/api/logs')
        .then(function (res) { return res.json(); })
        .then(function (files) {
          if (!sourceSelect || !Array.isArray(files)) return;
          sourceSelect.innerHTML = '<option value="live">🔴 Live Stream (Current)</option>';
          files.forEach(function (f) {
            var opt = document.createElement('option');
            opt.value = f.name;
            var kb = (f.size_bytes / 1024).toFixed(1);
            opt.textContent = f.name + ' (' + kb + ' KB)' + (f.is_current ? ' [Today]' : '');
            sourceSelect.appendChild(opt);
          });
        })
        .catch(function () {});
    }

    if (sourceSelect) {
      sourceSelect.addEventListener('change', function () {
        if (sourceSelect.value === 'live') {
          connectLiveStream();
        } else {
          loadHistoricFile(sourceSelect.value);
        }
      });
    }

    if (levelFilters) {
      levelFilters.addEventListener('click', function (e) {
        var btn = e.target.closest('.btn-chip');
        if (!btn) return;
        levelFilters.querySelectorAll('.btn-chip').forEach(function (b) { b.classList.remove('active'); });
        btn.classList.add('active');
        activeLevel = btn.dataset.level || 'ALL';
        refreshConsoleView();
      });
    }

    if (searchInput) {
      searchInput.addEventListener('input', function () {
        activeSearch = searchInput.value.trim();
        refreshConsoleView();
      });
    }

    function copyText(str, btn) {
      var done = function () {
        var old = btn.textContent;
        btn.textContent = 'Copied!';
        setTimeout(function () { btn.textContent = old; }, 1200);
      };
      if (navigator.clipboard) {
        navigator.clipboard.writeText(str).then(done, done);
      } else {
        done();
      }
    }

    if (copy50Btn) {
      copy50Btn.addEventListener('click', function () {
        var matching = currentEntries.filter(matchesFilter);
        var slice = matching.slice(-50).map(function (e) { return e.raw; }).join('\n');
        copyText(slice, copy50Btn);
      });
    }

    if (copyAllBtn) {
      copyAllBtn.addEventListener('click', function () {
        var matching = currentEntries.filter(matchesFilter);
        var all = matching.map(function (e) { return e.raw; }).join('\n');
        copyText(all, copyAllBtn);
      });
    }

    if (downloadBtn) {
      downloadBtn.addEventListener('click', function () {
        if (sourceSelect && sourceSelect.value !== 'live') {
          window.location.href = '/api/logs/download?file=' + encodeURIComponent(sourceSelect.value);
        } else {
          var text = currentEntries.map(function (e) { return e.raw; }).join('\n');
          var blob = new Blob([text], { type: 'text/plain;charset=utf-8' });
          var a = document.createElement('a');
          a.href = URL.createObjectURL(blob);
          a.download = 'rtosu-live-' + new Date().toISOString().slice(0, 10) + '.log';
          a.click();
        }
      });
    }

    if (clearBtn) {
      clearBtn.addEventListener('click', function () {
        currentEntries = [];
        refreshConsoleView();
      });
    }

    if (autoscrollCheck) {
      autoscrollCheck.addEventListener('change', function () {
        if (autoscrollCheck.checked) {
          userScrolledUp = false;
          scrollToBottom();
        }
      });
    }

    fetchLogFileList();
    connectLiveStream();
  })();

  // ---- JSON API Inspector & Live Poller Controller ----
  (function () {
    var modal = document.getElementById('json-inspector-modal');
    if (!modal) return;

    var backdrop = document.getElementById('json-modal-backdrop');
    var closeBtn = document.getElementById('json-close-btn');
    var endpointSelect = document.getElementById('json-endpoint-select');
    var customInput = document.getElementById('json-endpoint-custom');
    var pollToggle = document.getElementById('json-poll-toggle');
    var pollLabel = document.getElementById('json-poll-label');
    var pollRateInput = document.getElementById('json-poll-rate');
    var fetchBtn = document.getElementById('json-fetch-btn');
    var searchInput = document.getElementById('json-search-input');
    var searchCount = document.getElementById('json-search-count');
    var viewModeGroup = document.getElementById('json-view-mode');
    var expandAllBtn = document.getElementById('json-expand-all');
    var collapseAllBtn = document.getElementById('json-collapse-all');
    var copyBtn = document.getElementById('json-copy-btn');
    var treeContainer = document.getElementById('json-tree-container');
    var rawContainer = document.getElementById('json-raw-container');
    var rawCode = document.getElementById('json-raw-code');
    var statusTag = document.getElementById('json-status-tag');
    var metaInfo = document.getElementById('json-meta-info');

    var isPolling = false;
    var pollTimer = null;
    var currentData = null;
    var activeViewMode = 'tree';
    var userNodeStates = {};
    var globalExpandOverride = null;

    function getActiveEndpoint() {
      if (endpointSelect.value === 'custom') {
        var v = customInput.value.trim();
        return v.length > 0 ? (v.startsWith('/') ? v : '/' + v) : '/json/v2';
      }
      return endpointSelect.value;
    }

    function openInspector(endpoint) {
      if (endpoint) {
        var found = false;
        for (var i = 0; i < endpointSelect.options.length; i++) {
          if (endpointSelect.options[i].value === endpoint) {
            endpointSelect.value = endpoint;
            customInput.style.display = 'none';
            found = true;
            break;
          }
        }
        if (!found) {
          endpointSelect.value = 'custom';
          customInput.value = endpoint;
          customInput.style.display = 'inline-block';
        }
      }
      modal.style.display = 'flex';
      fetchEndpoint(getActiveEndpoint(), true);
    }

    function closeInspector() {
      modal.style.display = 'none';
      stopPolling();
    }

    function startPolling() {
      isPolling = true;
      pollToggle.classList.add('active');
      pollLabel.textContent = 'Live Poll: ON';
      fetchEndpoint(getActiveEndpoint(), false);
    }

    function stopPolling() {
      isPolling = false;
      if (pollTimer) {
        clearTimeout(pollTimer);
        pollTimer = null;
      }
      pollToggle.classList.remove('active');
      pollLabel.textContent = 'Live Poll: OFF';
    }

    function scheduleNextPoll() {
      if (!isPolling) return;
      if (pollTimer) clearTimeout(pollTimer);
      var rateSec = parseFloat(pollRateInput.value) || 0.2;
      if (rateSec < 0.05) rateSec = 0.05;
      if (rateSec > 60) rateSec = 60;
      var intervalMs = Math.round(rateSec * 1000);
      pollTimer = setTimeout(function () {
        if (!isPolling) return;
        fetchEndpoint(getActiveEndpoint(), false);
      }, intervalMs);
    }

    function formatBytes(bytes) {
      if (bytes < 1024) return bytes + ' B';
      if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
      return (bytes / (1024 * 1024)).toFixed(2) + ' MB';
    }

    function fetchEndpoint(url, forceRebuild) {
      var startTime = performance.now();
      statusTag.textContent = 'Fetching...';
      statusTag.className = 'badge';

      fetch(url, { cache: 'no-store' })
        .then(function (res) {
          var latencyMs = Math.round(performance.now() - startTime);
          var statusText = res.status + ' ' + (res.statusText || (res.ok ? 'OK' : 'Error'));
          statusTag.textContent = statusText;
          statusTag.className = 'badge ' + (res.ok ? 'live-badge' : 'err');

          return res.text().then(function (text) {
            var byteLength = new Blob([text]).size;
            var timeStr = new Date().toLocaleTimeString();
            metaInfo.textContent = latencyMs + ' ms \u2022 ' + formatBytes(byteLength) + ' \u2022 ' + timeStr;

            var data;
            try {
              data = JSON.parse(text);
            } catch (err) {
              data = { _raw_response: text, _error: 'Invalid JSON: ' + err.message };
            }

            rawCode.textContent = JSON.stringify(data, null, 2);

            if (forceRebuild || !currentData) {
              currentData = data;
              rebuildTree();
            } else {
              updateTreeOrRebuild(data);
            }

            if (searchInput.value.trim().length > 0) {
              applySearchFilter(searchInput.value.trim());
            }

            if (isPolling) {
              scheduleNextPoll();
            }
          });
        })
        .catch(function (err) {
          statusTag.textContent = 'Fetch Failed';
          statusTag.className = 'badge err';
          metaInfo.textContent = err.message;
          if (isPolling) {
            scheduleNextPoll();
          }
        });
    }

    function formatPrimitive(val) {
      if (val === null) return 'null';
      if (typeof val === 'string') return JSON.stringify(val);
      if (typeof val === 'number') return String(val);
      if (typeof val === 'boolean') return val ? 'true' : 'false';
      return String(val);
    }

    function getValueClass(val) {
      if (val === null) return 'json-null';
      if (typeof val === 'string') return 'json-string';
      if (typeof val === 'number') return 'json-number';
      if (typeof val === 'boolean') return 'json-bool';
      return 'json-other';
    }

    function getValueByPath(obj, path) {
      if (!obj || !path) return obj;
      var parts = path.split('.');
      var cur = obj;
      for (var i = 0; i < parts.length; i++) {
        var p = parts[i];
        var arrMatch = p.match(/^(\w+)\[(\d+)\]$/);
        if (arrMatch) {
          var key = arrMatch[1];
          var idx = parseInt(arrMatch[2], 10);
          if (!cur || cur[key] === undefined || cur[key][idx] === undefined) return undefined;
          cur = cur[key][idx];
        } else {
          var bareArr = p.match(/^\[(\d+)\]$/);
          if (bareArr) {
            var bIdx = parseInt(bareArr[1], 10);
            if (!cur || cur[bIdx] === undefined) return undefined;
            cur = cur[bIdx];
          } else {
            if (!cur || cur[p] === undefined) return undefined;
            cur = cur[p];
          }
        }
      }
      return cur;
    }

    function canReconcileInPlace(oldData, newData) {
      if (typeof oldData !== typeof newData || oldData === null || newData === null) return false;
      if (Array.isArray(oldData) !== Array.isArray(newData)) return false;
      if (Array.isArray(oldData)) {
        return oldData.length === newData.length;
      }
      if (typeof oldData === 'object') {
        var oldKeys = Object.keys(oldData);
        var newKeys = Object.keys(newData);
        if (oldKeys.length !== newKeys.length) return false;
        for (var i = 0; i < oldKeys.length; i++) {
          if (oldKeys[i] !== newKeys[i]) return false;
        }
        return true;
      }
      return true;
    }

    function updateTreeOrRebuild(newData) {
      if (!canReconcileInPlace(currentData, newData) || !treeContainer.firstElementChild) {
        currentData = newData;
        var savedScroll = treeContainer.scrollTop;
        rebuildTree();
        treeContainer.scrollTop = savedScroll;
        return;
      }
      currentData = newData;
      var valSpans = treeContainer.querySelectorAll('[data-val-path]');
      for (var s = 0; s < valSpans.length; s++) {
        var span = valSpans[s];
        var p = span.getAttribute('data-val-path');
        var val = getValueByPath(newData, p);
        if (val !== undefined) {
          var text = formatPrimitive(val);
          if (span.textContent !== text) {
            span.textContent = text;
            span.className = 'json-val ' + getValueClass(val) + ' json-val-flash';
            (function (el) {
              setTimeout(function () { el.classList.remove('json-val-flash'); }, 400);
            })(span);
          }
        }
      }
    }

    function buildTreeNode(key, value, path, depth) {
      var isArray = Array.isArray(value);
      var isObject = value !== null && typeof value === 'object';
      var nodeEl = document.createElement('div');
      nodeEl.className = 'json-node' + (isObject ? ' json-node-complex' : '');
      nodeEl.setAttribute('data-path', path);

      if (isObject) {
        var keys = Object.keys(value);
        var caret = document.createElement('span');
        caret.className = 'json-caret';
        caret.textContent = '\u25bc';
        nodeEl.appendChild(caret);

        var keySpan = null;
        if (key !== null) {
          keySpan = document.createElement('span');
          keySpan.className = 'json-key';
          keySpan.textContent = JSON.stringify(key);
          nodeEl.appendChild(keySpan);
          nodeEl.appendChild(document.createTextNode(': '));
        }

        var openBracket = document.createElement('span');
        openBracket.className = 'json-bracket';
        openBracket.textContent = isArray ? '[' : '{';
        nodeEl.appendChild(openBracket);

        var countBadge = document.createElement('span');
        countBadge.className = 'json-item-count';
        countBadge.textContent = isArray ? (' ' + keys.length + ' items') : (' ' + keys.length + ' keys');
        nodeEl.appendChild(countBadge);

        var childrenEl = document.createElement('div');
        childrenEl.className = 'json-children';
        childrenEl.setAttribute('data-children-path', path);

        var collapsed = false;
        if (userNodeStates[path] !== undefined) {
          collapsed = userNodeStates[path];
        } else if (globalExpandOverride !== null) {
          collapsed = !globalExpandOverride;
        } else {
          collapsed = depth >= 1;
        }

        if (collapsed) {
          caret.classList.add('collapsed');
          childrenEl.classList.add('collapsed');
          childrenEl.setAttribute('hidden', 'until-found');
        }

        childrenEl.addEventListener('beforematch', function () {
          userNodeStates[path] = false;
          caret.classList.remove('collapsed');
          childrenEl.classList.remove('collapsed');
          childrenEl.removeAttribute('hidden');
        });

        var toggle = function (e) {
          if (e) e.stopPropagation();
          var isNowCollapsed = !childrenEl.classList.contains('collapsed');
          userNodeStates[path] = isNowCollapsed;
          if (isNowCollapsed) {
            caret.classList.add('collapsed');
            childrenEl.classList.add('collapsed');
            childrenEl.setAttribute('hidden', 'until-found');
          } else {
            caret.classList.remove('collapsed');
            childrenEl.classList.remove('collapsed');
            childrenEl.removeAttribute('hidden');
          }
        };

        caret.addEventListener('click', toggle);
        if (keySpan) {
          keySpan.addEventListener('click', toggle);
        }

        for (var i = 0; i < keys.length; i++) {
          var k = keys[i];
          var childPath = path ? (isArray ? (path + '[' + k + ']') : (path + '.' + k)) : (isArray ? ('[' + k + ']') : k);
          var childNode = buildTreeNode(isArray ? null : k, value[k], childPath, depth + 1);
          childrenEl.appendChild(childNode);
        }
        nodeEl.appendChild(childrenEl);

        var closeBracket = document.createElement('span');
        closeBracket.className = 'json-bracket';
        closeBracket.textContent = isArray ? ']' : '}';
        nodeEl.appendChild(closeBracket);
      } else {
        if (key !== null) {
          var kSpan = document.createElement('span');
          kSpan.className = 'json-key';
          kSpan.textContent = JSON.stringify(key);
          nodeEl.appendChild(kSpan);
          nodeEl.appendChild(document.createTextNode(': '));
        }
        var valSpan = document.createElement('span');
        valSpan.className = 'json-val ' + getValueClass(value);
        valSpan.setAttribute('data-val-path', path);
        valSpan.textContent = formatPrimitive(value);
        nodeEl.appendChild(valSpan);
      }

      return nodeEl;
    }

    function rebuildTree() {
      treeContainer.innerHTML = '';
      if (currentData === null || currentData === undefined) {
        treeContainer.innerHTML = '<div class="json-placeholder">No data to display.</div>';
        return;
      }
      var rootNode = buildTreeNode(null, currentData, '', 0);
      treeContainer.appendChild(rootNode);
    }

    function applySearchFilter(query) {
      var allMatches = treeContainer.querySelectorAll('.json-search-match');
      for (var m = 0; m < allMatches.length; m++) {
        allMatches[m].classList.remove('json-search-match');
      }

      if (!query) {
        searchCount.textContent = '';
        return;
      }

      var q = query.toLowerCase();
      var nodes = treeContainer.querySelectorAll('.json-node');
      var matchCount = 0;

      for (var n = 0; n < nodes.length; n++) {
        var node = nodes[n];
        var keyEl = node.querySelector(':scope > .json-key');
        var valEl = node.querySelector(':scope > .json-val');
        var matched = false;

        if (keyEl && keyEl.textContent.toLowerCase().indexOf(q) !== -1) {
          keyEl.classList.add('json-search-match');
          matched = true;
        }
        if (valEl && valEl.textContent.toLowerCase().indexOf(q) !== -1) {
          valEl.classList.add('json-search-match');
          matched = true;
        }

        if (matched) {
          matchCount++;
          var cur = node.parentElement;
          while (cur && cur !== treeContainer) {
            if (cur.classList.contains('json-children') && cur.classList.contains('collapsed')) {
              cur.classList.remove('collapsed');
              cur.removeAttribute('hidden');
              var parentCaret = cur.parentElement.querySelector(':scope > .json-caret');
              if (parentCaret) parentCaret.classList.remove('collapsed');
            }
            cur = cur.parentElement;
          }
        }
      }

      searchCount.textContent = matchCount + (matchCount === 1 ? ' match' : ' matches');
    }

    // Modal Events
    backdrop.addEventListener('click', closeInspector);
    closeBtn.addEventListener('click', closeInspector);

    window.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && modal.style.display !== 'none') {
        closeInspector();
      }
    });

    endpointSelect.addEventListener('change', function () {
      if (endpointSelect.value === 'custom') {
        customInput.style.display = 'inline-block';
        customInput.focus();
      } else {
        customInput.style.display = 'none';
        userNodeStates = {};
        fetchEndpoint(endpointSelect.value, true);
      }
    });

    customInput.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') {
        userNodeStates = {};
        fetchEndpoint(getActiveEndpoint(), true);
      }
    });

    pollToggle.addEventListener('click', function () {
      if (isPolling) {
        stopPolling();
      } else {
        startPolling();
      }
    });

    pollRateInput.addEventListener('change', function () {
      if (isPolling) {
        scheduleNextPoll();
      }
    });

    fetchBtn.addEventListener('click', function () {
      fetchEndpoint(getActiveEndpoint(), false);
    });

    searchInput.addEventListener('input', function () {
      applySearchFilter(searchInput.value.trim());
    });

    viewModeGroup.addEventListener('click', function (e) {
      var btn = e.target.closest('.btn-chip');
      if (!btn) return;
      viewModeGroup.querySelectorAll('.btn-chip').forEach(function (b) { b.classList.remove('active'); });
      btn.classList.add('active');
      activeViewMode = btn.getAttribute('data-mode') || 'tree';

      if (activeViewMode === 'raw') {
        treeContainer.style.display = 'none';
        rawContainer.style.display = 'block';
      } else {
        treeContainer.style.display = 'block';
        rawContainer.style.display = 'none';
      }
    });

    expandAllBtn.addEventListener('click', function () {
      globalExpandOverride = true;
      userNodeStates = {};
      var carets = treeContainer.querySelectorAll('.json-caret');
      var children = treeContainer.querySelectorAll('.json-children');
      for (var i = 0; i < carets.length; i++) carets[i].classList.remove('collapsed');
      for (var j = 0; j < children.length; j++) {
        children[j].classList.remove('collapsed');
        children[j].removeAttribute('hidden');
      }
    });

    collapseAllBtn.addEventListener('click', function () {
      globalExpandOverride = false;
      userNodeStates = {};
      var carets = treeContainer.querySelectorAll('.json-caret');
      var children = treeContainer.querySelectorAll('.json-children');
      for (var i = 0; i < carets.length; i++) carets[i].classList.add('collapsed');
      for (var j = 0; j < children.length; j++) {
        children[j].classList.add('collapsed');
        children[j].setAttribute('hidden', 'until-found');
      }
    });

    copyBtn.addEventListener('click', function () {
      var str = JSON.stringify(currentData, null, 2);
      var done = function () {
        var old = copyBtn.textContent;
        copyBtn.textContent = 'Copied!';
        setTimeout(function () { copyBtn.textContent = old; }, 1200);
      };
      if (navigator.clipboard) {
        navigator.clipboard.writeText(str).then(done, done);
      } else {
        done();
      }
    });

    // Intercept clicks on inspect buttons & inspectable endpoint links
    document.addEventListener('click', function (e) {
      var inspectTrigger = e.target.closest('[data-inspect], .btn-inspect');
      if (inspectTrigger) {
        if (e.ctrlKey || e.metaKey || e.shiftKey) return;
        e.preventDefault();
        var targetPath = inspectTrigger.getAttribute('data-inspect') ||
                         inspectTrigger.getAttribute('data-endpoint') ||
                         inspectTrigger.getAttribute('href');
        if (targetPath) {
          openInspector(targetPath);
        }
      }
    });
  })();

  if (meta.writable) {
    setStatus('Ready. ' + controls.length + ' settings loaded.', '');
  } else if (meta.writable_reason === 'config_read_only') {
    setStatus('Editing is disabled: the config file is not writable.', 'err');
  } else {
    setStatus('Read-only view: saving is disabled on this machine.', '');
  }
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique directory per test, so the parallel test runner cannot have two
    /// tests writing the same config file.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rtosu-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the test directory");
        dir
    }

    fn store_for(name: &str) -> SettingsStore {
        SettingsStore::new(temp_dir(name).join("config.toml"), AppConfig::default())
            .expect("the default config is valid")
    }

    /// A TOML value as the JSON a patch would carry.
    fn json_value_of(value: &toml::Value) -> serde_json::Value {
        match value {
            toml::Value::Boolean(flag) => serde_json::Value::Bool(*flag),
            toml::Value::Integer(number) => serde_json::json!(*number),
            toml::Value::Float(number) => serde_json::json!(*number),
            toml::Value::String(text) => serde_json::Value::String(text.clone()),
            toml::Value::Table(table) => serde_json::Value::Object(
                table
                    .iter()
                    .map(|(key, value)| (key.clone(), json_value_of(value)))
                    .collect(),
            ),
            other => panic!("unexpected value in the config: {other:?}"),
        }
    }

    /// Every spec must be readable, patchable, and round-trip its own value.
    ///
    /// This is the guard on "the field table is the only list of settings": the
    /// form, the patcher and the config writer all read `FIELD_SPECS`, so a spec
    /// whose key the config does not carry, or which the patcher cannot set,
    /// would be a row the page renders and the save silently drops.
    #[test]
    fn every_field_spec_can_be_patched_and_read_back() {
        let store = store_for("every-field");
        let before = field_values(&store.config()).expect("every spec must resolve its key");
        assert_eq!(before.len(), FIELD_SPECS.len());

        for spec in FIELD_SPECS {
            assert!(
                spec.key.split_once('.').is_some_and(|(section, leaf)| {
                    !section.is_empty() && !leaf.is_empty() && !leaf.contains('.')
                }),
                "{} must be a section.key path",
                spec.key
            );
            assert!(
                field_spec(spec.key).is_some(),
                "{} is looked up by key",
                spec.key
            );
        }

        for field in &before {
            let patch = serde_json::json!({ field.spec.key: json_value_of(&field.value) });
            let outcome = store
                .apply_patch(&patch)
                .unwrap_or_else(|err| panic!("{}: {err:#}", field.spec.key));
            assert_eq!(outcome.applied, vec![field.spec.key.to_string()]);

            // Sending the current value back must merge to the same config, key
            // by key: that is what makes "only the changed leaves are rewritten"
            // in the config writer true.
            let after = field_values(&outcome.config).expect("the merged config must be readable");
            for (before, after) in before.iter().zip(after.iter()) {
                assert_eq!(
                    before.value, after.value,
                    "{} must round-trip unchanged",
                    before.spec.key
                );
            }
        }
    }

    /// Every rejection names the offending key, because the page prints the
    /// server's message next to the form.
    #[test]
    fn a_rejected_patch_names_the_key_it_refused() {
        let store = store_for("rejections");

        let cases: &[(&str, serde_json::Value, &str)] = &[
            (
                "poll.poll_rate_hz",
                serde_json::json!(0),
                "must be at least 1.0",
            ),
            (
                "poll.poll_rate_hz",
                serde_json::json!(121),
                "must be at most 120.0",
            ),
            (
                "server.port",
                serde_json::json!(80),
                "must be at least 1024.0",
            ),
            (
                "features.enable_pp",
                serde_json::json!("yes"),
                "expected true or false",
            ),
            (
                "logging.level",
                serde_json::json!("verbose"),
                "must be one of",
            ),
            // A cross-field rule from `AppConfig::validate`, which is the same
            // validator the config file goes through at startup.
            (
                "server.enable_http",
                serde_json::json!(false),
                "requires server.enable_http",
            ),
            (
                "scoring.mod_multipliers",
                serde_json::json!({ "DTNC": 2.0 }),
                "unknown mod acronym 'DTNC'",
            ),
            (
                "scoring.mod_multipliers",
                serde_json::json!({ "EZ": 200.0 }),
                "must be between 0.01 and 100.0",
            ),
        ];

        for (key, value, expected) in cases {
            let patch = serde_json::json!({ *key: value.clone() });
            let error = store
                .apply_patch(&patch)
                .expect_err(&format!("{key} must be rejected"));
            let message = format!("{error:#}");
            assert!(
                message.starts_with(key),
                "{key} must be named first, got: {message}"
            );
            assert!(
                !message.contains(&format!("{key}: {key}:")),
                "{key} must be named once, got: {message}"
            );
            assert!(
                message.contains(expected),
                "{key} must explain itself ({expected}), got: {message}"
            );
        }

        // An unknown key, a non-object patch and an empty patch are refused too.
        let error = store
            .apply_patch(&serde_json::json!({ "server.overlay_dir": "x" }))
            .expect_err("an unknown key must be rejected");
        assert!(format!("{error:#}").contains("unknown key 'server.overlay_dir'"));

        let error = store
            .apply_patch(&serde_json::json!([1, 2]))
            .expect_err("a non-object patch must be rejected");
        assert!(format!("{error:#}").contains("must be a JSON object"));

        let error = store
            .apply_patch(&serde_json::json!({}))
            .expect_err("an empty patch must be rejected");
        assert!(format!("{error:#}").contains("nothing to change"));

        // Nothing above may have touched the store or the file.
        assert_eq!(store.config().poll.poll_rate_hz, 60);
        assert!(
            !store.path().exists(),
            "a rejected patch writes nothing at all"
        );
    }

    /// A valid patch changes exactly the keys it names, and the live subset
    /// follows when the change is committed.
    #[test]
    fn a_committed_patch_publishes_the_live_subset() {
        let store = store_for("commit");
        let mut live_rx = store.subscribe();
        assert!(!live_rx.has_changed().unwrap_or(false));

        let outcome = store
            .apply_patch(&serde_json::json!({
                "features.enable_pp": false,
                "poll.poll_rate_hz": 120,
            }))
            .expect("both keys are valid");
        assert_eq!(
            outcome.applied,
            vec![
                "features.enable_pp".to_string(),
                "poll.poll_rate_hz".to_string()
            ]
        );
        assert!(
            outcome.restart_required.is_empty(),
            "both keys apply without a restart"
        );
        assert!(!outcome.config.features.enable_pp);
        assert_eq!(outcome.config.poll.poll_rate_hz, 120);
        assert_eq!(store.config().poll.poll_rate_hz, 60, "not committed yet");

        store.commit(outcome.config).expect("commit");
        assert_eq!(store.config().poll.poll_rate_hz, 120);
        assert!(live_rx.has_changed().unwrap_or(false));
        let live = live_rx.borrow_and_update().clone();
        assert_eq!(live.poll_interval, Duration::from_millis(8));
        assert!(!live.enable_pp);
    }

    /// A restart-only key is reported as such, and lands in the config anyway.
    #[test]
    fn a_restart_only_key_is_reported_and_persisted() {
        let store = store_for("restart");
        let outcome = store
            .apply_patch(&serde_json::json!({ "logging.level": "debug" }))
            .expect("debug is a valid level");
        assert_eq!(outcome.restart_required, vec!["logging.level".to_string()]);
        assert_eq!(outcome.config.logging.level, "debug");
        assert!(restart_required_keys().contains(&"logging.level"));
        assert!(!restart_required_keys().contains(&"poll.poll_rate_hz"));
    }

    /// The writer patches text, so every comment, every key the patch does not
    /// mention, and every line's position survives a save.
    #[test]
    fn persisting_keeps_every_comment_and_rewrites_only_the_edited_lines() {
        let dir = temp_dir("comments");
        let path = dir.join("config.toml");
        let template = AppConfig::generate_documented_template();
        fs::write(&path, template).expect("write the template");

        let mut config = AppConfig::default();
        config.poll.poll_rate_hz = 120;
        config.features.enable_pp = false;
        config.logging.level = "debug".to_string();
        config
            .save_preserving_comments(&path, None)
            .expect("the save must succeed");

        let after = fs::read_to_string(&path).expect("read the file back");
        let parsed: AppConfig = toml::from_str(&after).expect("the file must still parse");
        parsed.validate().expect("the file must still validate");
        assert_eq!(parsed.poll.poll_rate_hz, 120);
        assert!(!parsed.features.enable_pp);
        assert_eq!(parsed.logging.level, "debug");

        // The comments are the reason the writer exists, so they are asserted on
        // directly rather than inferred from the file parsing.
        assert!(after.contains("# A high-performance native Rust memory reader"));
        assert!(after.contains("# Default: 60"));
        assert_eq!(
            after.lines().count(),
            template.lines().count(),
            "a save must not add or drop a line"
        );

        let before_lines: Vec<&str> = template.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        let changed: Vec<&str> = before_lines
            .iter()
            .zip(after_lines.iter())
            .filter(|(before, after)| before != after)
            .map(|(_, after)| *after)
            .collect();
        assert_eq!(
            changed.len(),
            3,
            "only the three edited leaves: {changed:?}"
        );
        for leaf in [
            "poll_rate_hz = 120",
            "enable_pp = false",
            "level = \"debug\"",
        ] {
            assert!(
                changed.contains(&leaf),
                "{leaf} must be one of the rewritten lines: {changed:?}"
            );
        }
    }

    /// A config written before a key or a whole section existed gains both on the
    /// first save, in the place the template would have put them.
    #[test]
    fn a_key_or_section_the_file_does_not_carry_is_appended_where_it_belongs() {
        let dir = temp_dir("append");
        let path = dir.join("config.toml");
        // No `[scoring]` at all, no `settings_write_local_only`, and none of the
        // other leaves: exactly what an older config looks like.
        let base = "[server]\nhost = \"127.0.0.1\"\n\n[poll]\npoll_rate_hz = 60\n\n[logging]\nmax_log_files = 7\n";
        fs::write(&path, base).expect("write the base config");

        AppConfig::default()
            .save_preserving_comments(&path, None)
            .expect("the save must succeed");
        let after = fs::read_to_string(&path).expect("read the file back");
        let parsed: AppConfig = toml::from_str(&after).expect("the file must still parse");
        parsed.validate().expect("the file must still validate");

        // The missing section is created with its header, before the section that
        // follows it in the canonical order.
        let scoring = after.find("[scoring]").expect("[scoring] must be added");
        let multipliers = after
            .find("mod_multipliers")
            .expect("its key must be added");
        let logging = after.find("[logging]").expect("[logging] is already there");
        assert!(
            scoring < multipliers,
            "the key must be inside its own section"
        );
        assert!(multipliers < logging, "[scoring] belongs before [logging]");

        // A key missing from a section that does exist is appended to that
        // section rather than to the end of the file.
        let server = after.find("[server]").expect("[server]");
        let guard = after
            .find("settings_write_local_only")
            .expect("the guard must be added");
        let poll = after.find("[poll]").expect("[poll]");
        assert!(server < guard && guard < poll, "appended inside [server]");

        // Every field now lives in the file, which is what the next save reads.
        for field in field_values(&parsed).expect("the merged config must be readable") {
            let (section, _leaf) = field.spec.key.split_once('.').expect("section.key");
            assert!(
                after.contains(&format!("[{section}]")),
                "{} needs its section",
                field.spec.key
            );
        }
    }

    /// A write that cannot complete leaves the previous file exactly as it was.
    #[test]
    fn a_save_that_cannot_be_written_leaves_the_old_file_untouched() {
        let dir = temp_dir("atomic");
        let path = dir.join("config.toml");
        let original = "[server]\nhost = \"127.0.0.1\"\n";
        fs::write(&path, original).expect("write the original");
        // Block the writer's temporary path, so the failure happens while the
        // real file is still untouched and the rename never runs.
        fs::create_dir_all(dir.join(".config.toml.tmp")).expect("create the blocker");

        let error = AppConfig::default()
            .save_preserving_comments(&path, None)
            .expect_err("the save must fail");
        assert!(
            format!("{error:#}").contains(".config.toml.tmp"),
            "the error must name the file it could not write, got: {error:#}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the old file must still be readable"),
            original,
            "a failed save must not touch the config"
        );
    }

    /// Only an `Origin` that names the loopback interface counts as local.
    #[test]
    fn only_a_loopback_origin_is_treated_as_local() {
        for origin in [
            "http://127.0.0.1",
            "http://127.0.0.1:24050",
            "http://localhost:24050",
            "https://localhost",
            "http://[::1]:24050",
        ] {
            assert!(origin_is_loopback(origin), "{origin} is a local origin");
        }

        // A LAN address, a foreign host, the literal `null` a sandboxed document
        // sends, a host that merely *starts with* a loopback address, and
        // anything that is not an HTTP origin at all.
        for origin in [
            "http://192.168.1.10:24050",
            "https://evil.example",
            "null",
            "http://127.0.0.1.evil.example",
            "file:///c:/config.toml",
            "",
        ] {
            assert!(
                !origin_is_loopback(origin),
                "{origin} must not count as local"
            );
        }
    }

    /// The write guard, as a table: the peer, the origin and the file's own
    /// writability decide, in that order.
    #[test]
    fn the_write_guard_refuses_remote_peers_and_foreign_origins() {
        let config = AppConfig::default();
        let writable: std::result::Result<(), String> = Ok(());

        assert_eq!(write_blocker(&config, true, None, writable.clone()), None);
        assert_eq!(
            write_blocker(
                &config,
                true,
                Some("http://localhost:24050"),
                writable.clone()
            ),
            None
        );
        assert_eq!(
            write_blocker(&config, false, None, writable.clone()),
            Some(WriteBlocker::SettingsWriteLocalOnly)
        );
        assert_eq!(
            write_blocker(&config, true, Some("http://evil.example"), writable.clone()),
            Some(WriteBlocker::SettingsWriteLocalOnly),
            "a page loaded on the viewing machine must not be able to post"
        );

        // Publishing the port deliberately accepts a LAN peer, and only then.
        let mut published = AppConfig::default();
        published.server.settings_write_local_only = false;
        assert_eq!(
            write_blocker(&published, false, Some("http://evil.example"), writable),
            None
        );

        // A config file that cannot be written is refused whatever the peer is,
        // because a change that cannot be persisted must not be accepted.
        assert_eq!(
            write_blocker(&published, true, None, Err("access denied".to_string())),
            Some(WriteBlocker::ConfigReadOnly)
        );

        // The reason strings are the ones `GET /api/settings` documents.
        assert_eq!(
            WriteBlocker::SettingsWriteLocalOnly.code(),
            "settings_write_local_only"
        );
        assert_eq!(WriteBlocker::ConfigReadOnly.code(), "config_read_only");
    }

    /// The landing page renders the form, the overlay cards and the endpoint
    /// list, and escapes everything that came off disk or out of the config.
    #[test]
    fn the_landing_page_renders_the_form_the_overlays_and_the_links() {
        let store = store_for("landing");
        let overlays = vec![Overlay {
            slug: "<script>alert(1)</script>".to_string(),
            dir: PathBuf::from("C:/overlays/evil"),
            index: PathBuf::from("C:/overlays/evil/index.html"),
            metadata: Default::default(),
        }];
        let root = PathBuf::from("browser_overlays");

        let html = landing_html(&LandingView {
            host: "127.0.0.1",
            port: 24050,
            config_path: Some(store.path()),
            settings: Some(settings_response(&store, true)),
            overlays: Some((&overlays, &root)),
            can_restart: true,
        });

        // The header, the save bar and the endpoints.
        assert!(html.contains("rtosu"));
        assert!(html.contains("127.0.0.1:24050"));
        assert!(html.contains("id=\"save\"") && html.contains("id=\"reset\""));
        assert!(html.contains("/api/settings"));
        for link in ["/json/v2", "/json/sc", "/overlays", "/health"] {
            assert!(html.contains(link), "{link} must be linked");
        }

        // The restart button is offered where the endpoint would honour it.
        assert!(
            html.contains("id=\"restart\""),
            "a local viewer gets Restart"
        );
        assert!(meta_restart_available(&html));

        // The header links the repository and the footer credits tosu, whose
        // API and overlay format this server reproduces.
        assert!(html.contains("https://github.com/Raregendary/rtosu-dataprovider"));
        assert!(html.contains("https://tosu.app/"));
        assert!(
            html.contains("<footer"),
            "the footer carrying those links must be there"
        );

        // A folder named like HTML cannot inject markup, and its URL is the
        // percent-encoded form the overlay routes expect.
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("/overlays/%3Cscript%3E"));

        // One control per spec, keyed the way the patcher expects: this is the
        // assertion that a new setting cannot appear in the file without
        // appearing in the form.
        for field in FIELD_SPECS {
            assert!(
                html.contains(&format!("data-key=\"{}\"", field.key)),
                "{} must have a control",
                field.key
            );
        }
        assert_eq!(
            html.matches("data-key=").count(),
            FIELD_SPECS.len(),
            "the form has one control per field and no strays"
        );

        // One collapsible `details` section per config section, opened and
        // closed, each with a `summary` row so it can be collapsed by click.
        assert_eq!(
            html.matches("<details class=\"section\"").count(),
            5,
            "five sections"
        );
        assert_eq!(
            html.matches("<details class=\"section\"").count(),
            html.matches("</details>").count(),
            "every section is closed"
        );
        assert_eq!(html.matches("<summary>").count(), 5, "each one collapses");
        for section in ["server", "poll", "features", "scoring", "logging"] {
            assert!(
                html.contains(&format!("id=\"sec-{section}\"")),
                "the [{section}] section must be there"
            );
        }

        // The metadata blob is JSON the page's script can parse, and the `<` it
        // may contain is escaped for the `<script>` element.
        let blob = html
            .split("<script id=\"rtosu-data\" type=\"application/json\">")
            .nth(1)
            .and_then(|rest| rest.split("</script>").next())
            .expect("the page must embed its metadata");
        assert!(!blob.contains('<'), "a raw < would end the element early");
        let meta: serde_json::Value = serde_json::from_str(blob).expect("the blob must be JSON");
        assert_eq!(meta["writable"], serde_json::json!(true));
        assert_eq!(
            meta["version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
        assert!(
            meta["restart_required"]
                .as_array()
                .expect("a key list")
                .iter()
                .any(|key| key == "server.port"),
            "the page has to know which keys need a restart"
        );

        // Without a store the page still renders, and says why it is read-only.
        let bare = landing_html(&LandingView {
            host: "",
            port: 0,
            config_path: None,
            settings: None,
            overlays: None,
            can_restart: false,
        });
        assert!(bare.contains("Settings are unavailable"));
        assert!(
            bare.contains("Browser overlays are off"),
            "a page without an overlay store still explains itself"
        );
        assert!(
            bare.contains("/overlays"),
            "the endpoint list is still there"
        );
        assert!(
            !bare.contains("id=\"restart\""),
            "no supervisor, no restart button -- a dead button is worse than none"
        );
        assert!(!meta_restart_available(&bare));

        // An empty overlay directory is the "no overlays yet" hint, not a panic
        // and not a blank section.
        let empty = landing_html(&LandingView {
            host: "0.0.0.0",
            port: 24050,
            config_path: Some(store.path()),
            settings: Some(settings_response(&store, true)),
            overlays: Some((&[], &root)),
            can_restart: true,
        });
        assert!(empty.contains("No overlays found in"));
        assert!(
            empty.contains("127.0.0.1:24050"),
            "a wildcard bind is displayed as the address a viewer can open"
        );
    }

    /// Reads `restart_available` back out of the page's own metadata blob.
    fn meta_restart_available(html: &str) -> bool {
        html.split("<script id=\"rtosu-data\" type=\"application/json\">")
            .nth(1)
            .and_then(|rest| rest.split("</script>").next())
            .and_then(|blob| serde_json::from_str::<serde_json::Value>(blob).ok())
            .is_some_and(|meta| meta["restart_available"] == serde_json::json!(true))
    }

    /// `GET /api/settings` round-trips: the `config` it carries deserialises
    /// back into an `AppConfig`, which is what lets the page use the JSON shape
    /// as the config shape.
    #[test]
    fn the_settings_response_carries_the_config_the_fields_and_the_write_flag() {
        let store = store_for("response");
        let response = settings_response(&store, true);
        assert!(response.writable);
        assert_eq!(response.writable_reason, None);
        assert_eq!(response.config_path, store.path().display().to_string());
        assert_eq!(response.fields.len(), FIELD_SPECS.len());
        assert!(response.restart_required_keys.contains(&"server.port"));
        assert_eq!(response.version, env!("CARGO_PKG_VERSION"));

        let json = serde_json::to_value(&response).expect("the response must serialise");
        for key in ["config", "fields", "writable", "config_path"] {
            assert!(json.get(key).is_some(), "{key} must be in the body");
        }
        let config: AppConfig = serde_json::from_value(json["config"].clone())
            .expect("the config must deserialise back");
        assert_eq!(config.poll.poll_rate_hz, store.config().poll.poll_rate_hz);
        assert!(config.server.settings_write_local_only);

        // A viewer that is not on this machine may look but not write, and the
        // reason is the documented code.
        let remote = settings_response(&store, false);
        assert!(!remote.writable);
        assert_eq!(
            remote.writable_reason,
            Some(WriteBlocker::SettingsWriteLocalOnly)
        );
        let json = serde_json::to_value(&remote).expect("the response must serialise");
        assert_eq!(
            json["writable_reason"],
            serde_json::json!("settings_write_local_only")
        );
    }

    /// The shipped template, and this repository's own `config.toml`, must
    /// document every key the page can edit.
    ///
    /// The form is built from `FIELD_SPECS`, so a key the template lacks is
    /// appended to a fresh install's file by its first save -- the file would
    /// document itself only after somebody changed something. `json_payload` was
    /// exactly that case until this page existed to notice it.
    #[test]
    fn the_shipped_template_documents_every_field() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml");
        let documents = [
            (
                "the documented template",
                AppConfig::generate_documented_template().to_string(),
            ),
            (
                "the repository config.toml",
                fs::read_to_string(&repository)
                    .unwrap_or_else(|err| panic!("reading {}: {err}", repository.display())),
            ),
        ];

        for (name, text) in documents {
            let parsed: AppConfig =
                toml::from_str(&text).unwrap_or_else(|err| panic!("{name} must parse: {err}"));
            for field in field_values(&parsed).expect("every spec must resolve its key") {
                let (section, leaf) = field.spec.key.split_once('.').expect("section.key");
                assert!(
                    text.contains(&format!("[{section}]")),
                    "{name} must document the [{section}] section"
                );
                assert!(
                    text.contains(&format!("{leaf} =")),
                    "{name} must document {}",
                    field.spec.key
                );
            }
        }
    }
}
