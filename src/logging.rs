use crate::config::LoggingConfig;
use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
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

    if config.log_to_file {
        let writer = DailyLogWriter::new("logs", config.max_log_files)?;
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(writer);

        tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .with(file_layer)
            .try_init()
            .ok();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
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
}
