use crate::config::LoggingConfig;
use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// A single structured log entry for live streaming and UI presentation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
    pub raw: String,
}

/// Metadata about a daily log file on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogFileInfo {
    pub name: String,
    pub date: String,
    pub size_bytes: u64,
    pub modified_secs: u64,
    pub is_current: bool,
}

/// Contents of a read log file, containing total lines and selected slice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogFileContent {
    pub name: String,
    pub total_lines: usize,
    pub returned_lines: usize,
    pub lines: Vec<String>,
}

/// In-memory ring buffer holding recent log entries and broadcasting them to live tail clients.
pub struct LogTailBuffer {
    capacity: usize,
    entries: Mutex<VecDeque<LogEntry>>,
    tx: broadcast::Sender<LogEntry>,
}

impl LogTailBuffer {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            capacity,
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
            tx,
        }
    }

    pub fn push(&self, entry: LogEntry) {
        let _ = self.tx.send(entry.clone());
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= self.capacity {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }

    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        if let Ok(entries) = self.entries.lock() {
            let start = entries.len().saturating_sub(limit);
            entries.iter().skip(start).cloned().collect()
        } else {
            Vec::new()
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }
}

static GLOBAL_LOG_BUFFER: OnceLock<Arc<LogTailBuffer>> = OnceLock::new();

/// Access the process-wide log buffer for live streaming and recent query.
pub fn global_log_buffer() -> Arc<LogTailBuffer> {
    GLOBAL_LOG_BUFFER
        .get_or_init(|| Arc::new(LogTailBuffer::new(500)))
        .clone()
}

/// Subscribe to live log entries.
pub fn get_log_receiver() -> broadcast::Receiver<LogEntry> {
    global_log_buffer().subscribe()
}

/// Retrieve the most recent log entries up to `limit`.
pub fn get_recent_logs(limit: usize) -> Vec<LogEntry> {
    global_log_buffer().recent(limit)
}

/// Writer attached to tracing fmt layer to capture and broadcast log messages in real-time.
#[derive(Clone)]
pub struct LogTailWriter {
    buffer: Arc<LogTailBuffer>,
}

impl LogTailWriter {
    pub fn new() -> Self {
        Self {
            buffer: global_log_buffer(),
        }
    }

    pub fn with_buffer(buffer: Arc<LogTailBuffer>) -> Self {
        Self { buffer }
    }

    fn process_line(&self, line: &str) {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return;
        }

        // Standard tracing format: `TIMESTAMP  LEVEL target: message`
        let mut tokens = trimmed.split_whitespace();
        let timestamp = tokens.next().unwrap_or("").to_string();
        let level = tokens.next().unwrap_or("INFO").to_string();

        let rem = trimmed
            .strip_prefix(&timestamp)
            .unwrap_or(trimmed)
            .trim_start();
        let rem = rem.strip_prefix(&level).unwrap_or(rem).trim_start();

        let (target, message) = if let Some((tgt, msg)) = rem.split_once(": ") {
            (tgt.trim().to_string(), msg.trim().to_string())
        } else if let Some((tgt, msg)) = rem.split_once(':') {
            (tgt.trim().to_string(), msg.trim().to_string())
        } else {
            ("rtosu".to_string(), rem.to_string())
        };

        let entry = LogEntry {
            timestamp: if timestamp.is_empty() {
                Local::now().to_rfc3339()
            } else {
                timestamp
            },
            level,
            target,
            message,
            raw: trimmed.to_string(),
        };

        self.buffer.push(entry);
    }
}

impl Write for LogTailWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(text) = std::str::from_utf8(buf) {
            for line in text.lines() {
                self.process_line(line);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogTailWriter {
    type Writer = LogTailWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Enumerate available log files in `logs_dir` matching `rtosu-*.log`.
/// Returns list ordered newest first.
pub fn list_log_files<P: AsRef<Path>>(logs_dir: P) -> Result<Vec<LogFileInfo>> {
    let dir = logs_dir.as_ref();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let today = Local::now().format("%Y-%m-%d").to_string();
    let mut files = Vec::new();

    for entry in fs::read_dir(dir).with_context(|| format!("reading logs dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with("rtosu-") && file_name.ends_with(".log") {
                    let meta = entry.metadata().ok();
                    let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified_secs = meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    let date = file_name
                        .strip_prefix("rtosu-")
                        .and_then(|s| s.strip_suffix(".log"))
                        .unwrap_or("")
                        .to_string();
                    let is_current = date == today;

                    files.push(LogFileInfo {
                        name: file_name.to_string(),
                        date,
                        size_bytes,
                        modified_secs,
                        is_current,
                    });
                }
            }
        }
    }

    // Sort descending by date (newest first)
    files.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| b.modified_secs.cmp(&a.modified_secs))
    });
    Ok(files)
}

/// Read lines from a specific log file in `logs_dir`.
/// Safe against path traversal and returns up to `tail_lines` from the end of the file.
pub fn read_log_file<P: AsRef<Path>>(
    logs_dir: P,
    filename: &str,
    tail_lines: Option<usize>,
) -> Result<LogFileContent> {
    if filename.contains('/')
        || filename.contains('\\')
        || filename.contains(':')
        || filename.contains("..")
    {
        anyhow::bail!("Invalid log filename: path traversal characters not permitted");
    }
    let p = Path::new(filename);
    let mut comps = p.components();
    match comps.next() {
        Some(std::path::Component::Normal(_)) if comps.next().is_none() => {}
        _ => anyhow::bail!("Invalid log filename: single normal component required"),
    }
    if !filename.starts_with("rtosu-") || !filename.ends_with(".log") {
        anyhow::bail!("Invalid log filename: must match 'rtosu-*.log'");
    }

    let dir = logs_dir.as_ref();
    let file_path = dir.join(filename);
    if !file_path.is_file() {
        anyhow::bail!("Log file not found: {}", filename);
    }

    let content = fs::read_to_string(&file_path)
        .with_context(|| format!("reading log file {}", file_path.display()))?;
    let all_lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let total_lines = all_lines.len();

    let limit = tail_lines.unwrap_or(500);
    let start = total_lines.saturating_sub(limit);
    let returned: Vec<String> = all_lines.into_iter().skip(start).collect();
    let returned_lines = returned.len();

    Ok(LogFileContent {
        name: filename.to_string(),
        total_lines,
        returned_lines,
        lines: returned,
    })
}

/// Prunes old log files in the given directory matching `rtosu-*.log`
/// so that at most `max_files` of the newest files are retained.
/// Returns the number of pruned files.
pub fn prune_old_log_files<P: AsRef<Path>>(logs_dir: P, max_files: usize) -> Result<usize> {
    let dir = logs_dir.as_ref();
    if !dir.exists() {
        return Ok(0);
    }

    let mut log_files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading logs dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with("rtosu-") && file_name.ends_with(".log") {
                    let mtime = entry.metadata().and_then(|m| m.modified()).ok();
                    log_files.push((file_name.to_string(), mtime, path));
                }
            }
        }
    }

    // Sort ascending by file name (ISO-8601 YYYY-MM-DD suffix is chronologically ordered)
    // with modified time fallback
    log_files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut deleted = 0;
    if log_files.len() > max_files {
        let to_remove = log_files.len() - max_files;
        for (_, _, path) in log_files.iter().take(to_remove) {
            if fs::remove_file(path).is_ok() {
                deleted += 1;
            }
        }
    }

    Ok(deleted)
}

struct DailyLogWriterInner {
    current_date: String,
    file: Option<File>,
}

/// A thread-safe rolling file writer that writes logs to `logs/rtosu-YYYY-MM-DD.log`.
/// On daily boundary crossings and startup, it automatically prunes logs exceeding `max_log_files`.
#[derive(Clone)]
pub struct DailyLogWriter {
    logs_dir: PathBuf,
    max_log_files: usize,
    inner: Arc<Mutex<DailyLogWriterInner>>,
}

impl DailyLogWriter {
    pub fn new<P: AsRef<Path>>(logs_dir: P, max_log_files: usize) -> Result<Self> {
        let logs_dir = logs_dir.as_ref().to_path_buf();
        fs::create_dir_all(&logs_dir)
            .with_context(|| format!("creating logs directory at {}", logs_dir.display()))?;

        // Prune any existing excess log files on startup
        let _ = prune_old_log_files(&logs_dir, max_log_files);

        Ok(Self {
            logs_dir,
            max_log_files,
            inner: Arc::new(Mutex::new(DailyLogWriterInner {
                current_date: String::new(),
                file: None,
            })),
        })
    }

    fn write_buf(&self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "poisoned mutex in DailyLogWriter")
        })?;

        let today = Local::now().format("%Y-%m-%d").to_string();

        if inner.file.is_none() || inner.current_date != today {
            let _ = fs::create_dir_all(&self.logs_dir);
            let file_name = format!("rtosu-{}.log", today);
            let file_path = self.logs_dir.join(file_name);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(file_path)?;

            inner.file = Some(file);
            inner.current_date = today;

            // Trigger retention prune on date rotation
            let _ = prune_old_log_files(&self.logs_dir, self.max_log_files);
        }

        if let Some(ref mut file) = inner.file {
            file.write(buf)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "log file was not opened",
            ))
        }
    }

    fn flush_inner(&self) -> io::Result<()> {
        let mut inner = self.inner.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "poisoned mutex in DailyLogWriter")
        })?;
        if let Some(ref mut file) = inner.file {
            file.flush()
        } else {
            Ok(())
        }
    }
}

impl Write for DailyLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_buf(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_inner()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DailyLogWriter {
    type Writer = DailyLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(windows)]
fn enable_ansi_support() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        if (mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0 {
            return true;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_ansi_support() -> bool {
    true
}

/// Initialize global tracing subscriber respecting logging configuration.
pub fn init_logging(config: &LoggingConfig) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.level.to_ascii_lowercase()));

    let ansi_enabled = enable_ansi_support();
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_ansi(ansi_enabled)
        .with_target(false);

    let tail_writer = LogTailWriter::new();
    let tail_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(tail_writer);

    if config.log_to_file {
        let writer = DailyLogWriter::new("logs", config.max_log_files)?;
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(writer);

        tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .with(tail_layer)
            .with(file_layer)
            .try_init()
            .ok();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .with(tail_layer)
            .try_init()
            .ok();
    }

    let level_lower = config.level.to_ascii_lowercase();
    if level_lower == "debug" || level_lower == "trace" {
        tracing::warn!(
            "Logging level is set to '{}'. High-volume logs may increase CPU usage and impact high-frequency poll timing.",
            level_lower
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_test_dir(name: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rtosu_test_{}_{}", name, timestamp));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_log_pruning_keeps_exact_max_files() {
        let dir = create_test_dir("prune_keep");

        // Create 10 daily log files
        for i in 1..=10 {
            let filename = format!("rtosu-2026-09-{:02}.log", i);
            fs::write(dir.join(filename), format!("log content {i}")).unwrap();
        }

        // Also create non-matching files
        fs::write(dir.join("other.log"), "ignore").unwrap();
        fs::write(dir.join("rtosu-test.txt"), "ignore").unwrap();

        // Prune to keep 7 files
        let pruned = prune_old_log_files(&dir, 7).unwrap();
        assert_eq!(pruned, 3);

        // Oldest 3 files (01, 02, 03) should be deleted
        assert!(!dir.join("rtosu-2026-09-01.log").exists());
        assert!(!dir.join("rtosu-2026-09-02.log").exists());
        assert!(!dir.join("rtosu-2026-09-03.log").exists());

        // Newest 7 files (04..10) must remain
        for i in 4..=10 {
            let filename = format!("rtosu-2026-09-{:02}.log", i);
            assert!(dir.join(filename).exists());
        }

        // Non-matching files must remain untouched
        assert!(dir.join("other.log").exists());
        assert!(dir.join("rtosu-test.txt").exists());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_log_pruning_noop_when_under_limit() {
        let dir = create_test_dir("prune_noop");

        for i in 1..=4 {
            let filename = format!("rtosu-2026-09-{:02}.log", i);
            fs::write(dir.join(filename), "content").unwrap();
        }

        let pruned = prune_old_log_files(&dir, 7).unwrap();
        assert_eq!(pruned, 0);

        for i in 1..=4 {
            let filename = format!("rtosu-2026-09-{:02}.log", i);
            assert!(dir.join(filename).exists());
        }

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_daily_log_writer_writes_and_flushes() {
        let dir = create_test_dir("writer");
        let mut writer = DailyLogWriter::new(&dir, 7).unwrap();

        writeln!(writer, "Test log message line 1").unwrap();
        writeln!(writer, "Test log message line 2").unwrap();
        writer.flush().unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let expected_file = dir.join(format!("rtosu-{}.log", today));
        assert!(expected_file.exists());

        let content = fs::read_to_string(&expected_file).unwrap();
        assert!(content.contains("Test log message line 1"));
        assert!(content.contains("Test log message line 2"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_list_and_read_log_files() {
        let dir = create_test_dir("list_read");

        let file1 = dir.join("rtosu-2026-09-28.log");
        let file2 = dir.join("rtosu-2026-09-29.log");
        fs::write(&file1, "Line 1\nLine 2\nLine 3\n").unwrap();
        fs::write(&file2, "Line A\nLine B\n").unwrap();
        fs::write(dir.join("ignore.txt"), "no").unwrap();

        let files = list_log_files(&dir).unwrap();
        assert_eq!(files.len(), 2);
        // Newest date first
        assert_eq!(files[0].name, "rtosu-2026-09-29.log");
        assert_eq!(files[1].name, "rtosu-2026-09-28.log");

        let content = read_log_file(&dir, "rtosu-2026-09-28.log", Some(2)).unwrap();
        assert_eq!(content.total_lines, 3);
        assert_eq!(content.returned_lines, 2);
        assert_eq!(content.lines, vec!["Line 2", "Line 3"]);

        // Path traversal rejection
        assert!(read_log_file(&dir, "../secret.log", None).is_err());
        assert!(read_log_file(&dir, "other.txt", None).is_err());
        assert!(read_log_file(&dir, "C:Cargo.toml", None).is_err());
        assert!(read_log_file(&dir, "C:rtosu-2026-09-28.log", None).is_err());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_log_tail_writer_parses_target_with_colons() {
        let buffer = Arc::new(LogTailBuffer::new(10));
        let writer = LogTailWriter::with_buffer(buffer.clone());
        let raw_line = "2026-10-02T02:50:05.502667Z  INFO rtosu_dataprovider::server: Listening on TCP socket 127.0.0.1:24099\n";
        writer.process_line(raw_line);

        let recent = buffer.recent(1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].level, "INFO");
        assert_eq!(recent[0].target, "rtosu_dataprovider::server");
        assert_eq!(recent[0].message, "Listening on TCP socket 127.0.0.1:24099");
    }

    #[test]
    fn test_log_tail_buffer_push_and_recent() {
        let buffer = LogTailBuffer::new(3);
        let mut rx = buffer.subscribe();

        let make_entry = |msg: &str| LogEntry {
            timestamp: "2026-10-01T00:00:00Z".to_string(),
            level: "INFO".to_string(),
            target: "test".to_string(),
            message: msg.to_string(),
            raw: msg.to_string(),
        };

        buffer.push(make_entry("m1"));
        buffer.push(make_entry("m2"));
        buffer.push(make_entry("m3"));
        buffer.push(make_entry("m4")); // drops m1

        let recent = buffer.recent(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message, "m2");
        assert_eq!(recent[1].message, "m3");
        assert_eq!(recent[2].message, "m4");

        // Broadcast received m1
        assert_eq!(rx.try_recv().unwrap().message, "m1");
    }
}
