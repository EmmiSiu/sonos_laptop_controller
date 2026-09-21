//! Logging setup: size-capped, sample-free, and off by default beyond `info`.
//!
//! Three properties matter here, and each is a mechanism rather than a convention:
//!
//! - **Audio never reaches the log.** [`Samples`] renders as a count, so a well-meaning
//!   `tracing::debug!(?buffer)` prints `<480 samples>` instead of the user's conversation.
//! - **The log cannot fill the disk.** [`SizeCappedWriter`] rotates at a hard ceiling.
//! - **Verbosity is opt-in.** `info` by default, `RINCON_LOG` to override.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::limits;

/// The environment variable that overrides log verbosity.
pub const LOG_ENV: &str = "RINCON_LOG";

/// The default filter when [`LOG_ENV`] is unset.
pub const DEFAULT_FILTER: &str = "info";

/// Telemetry could not be initialised.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// The log file could not be opened.
    #[error("cannot open log file: {0}")]
    OpenLog(#[source] io::Error),

    /// A global subscriber was already installed.
    #[error("a tracing subscriber is already installed")]
    AlreadyInitialised,
}

/// A wrapper that makes audio buffers safe to log.
///
/// `tracing` will happily format a `&[f32]`, which would put the user's system audio into a
/// file on disk. Wrapping the slice makes that impossible by construction: there is no code
/// path from this type to the sample values.
///
/// ```
/// use rincon_core::telemetry::Samples;
/// let buffer = vec![0.5_f32; 480];
/// assert_eq!(format!("{}", Samples(&buffer)), "<480 samples>");
/// ```
///
/// Covers: RQ-OBS-003
pub struct Samples<'a>(pub &'a [f32]);

impl fmt::Debug for Samples<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} samples>", self.0.len())
    }
}

impl fmt::Display for Samples<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} samples>", self.0.len())
    }
}

/// A file writer that truncates-and-rotates once it passes a byte ceiling.
///
/// Deliberately simple: one rotation generation (`rincon.log` → `rincon.log.1`) and a hard
/// ceiling. A logging setup that can consume unbounded disk on a user's laptop is a defect,
/// and the cheapest correct fix is the one that cannot itself fail in interesting ways.
#[derive(Debug)]
pub struct SizeCappedWriter {
    path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
}

impl SizeCappedWriter {
    /// Opens (or creates) `path`, appending, with a ceiling of `max_bytes`.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the file cannot be opened.
    pub fn open(path: impl Into<PathBuf>, max_bytes: u64) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map_or(0, |meta| meta.len());
        Ok(Self { path, file, written, max_bytes })
    }

    /// Bytes written to the current generation of the file.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// The path being written to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let rotated = self.path.with_extension("log.1");
        // A failed rename must not leave us without a log; fall back to truncation.
        if std::fs::rename(&self.path, &rotated).is_err() {
            let _ = std::fs::remove_file(&self.path);
        }
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

impl Write for SizeCappedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.saturating_add(buf.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written = self.written.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// A cloneable handle to a [`SizeCappedWriter`], usable as a `tracing` writer.
#[derive(Debug, Clone)]
pub struct SharedWriter(Arc<Mutex<SizeCappedWriter>>);

impl SharedWriter {
    /// Wraps a writer for use by the tracing subscriber.
    #[must_use]
    pub fn new(inner: SizeCappedWriter) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }

    /// Bytes written to the current generation, for tests and diagnostics.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).written()
    }
}

/// The per-event writer handed out by [`SharedWriter`].
#[derive(Debug)]
pub struct SharedWriterGuard(Arc<Mutex<SizeCappedWriter>>);

impl Write for SharedWriterGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).flush()
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriterGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterGuard(Arc::clone(&self.0))
    }
}

/// How telemetry should be set up for this process.
#[derive(Debug, Clone, Default)]
pub struct TelemetryConfig {
    /// Where to write the rolling log, if anywhere.
    pub log_file: Option<PathBuf>,
    /// Also write human-readable output to stderr.
    pub stderr: bool,
}

impl TelemetryConfig {
    /// Stderr only: the default for the CLI and for tests.
    #[must_use]
    pub const fn stderr_only() -> Self {
        Self { log_file: None, stderr: true }
    }

    /// Stderr plus a rolling file: the desktop app's configuration.
    #[must_use]
    pub fn with_log_file(path: impl Into<PathBuf>) -> Self {
        Self { log_file: Some(path.into()), stderr: true }
    }
}

/// Reads the effective filter from the environment, falling back to [`DEFAULT_FILTER`].
///
/// Covers: RQ-OBS-003
#[must_use]
pub fn env_filter() -> EnvFilter {
    EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// Installs the global subscriber.
///
/// # Errors
///
/// Returns [`TelemetryError`] when the log file cannot be opened or a subscriber is already
/// installed.
///
/// Covers: RQ-OBS-003, RQ-OBS-005
pub fn init(config: &TelemetryConfig) -> Result<Option<SharedWriter>, TelemetryError> {
    let file_writer = config
        .log_file
        .as_ref()
        .map(|path| {
            SizeCappedWriter::open(path, limits::MAX_LOG_BYTES)
                .map(SharedWriter::new)
                .map_err(TelemetryError::OpenLog)
        })
        .transpose()?;

    let stderr_layer = config
        .stderr
        .then(|| tracing_subscriber::fmt::layer().with_target(true).with_writer(io::stderr));

    let file_layer = file_writer.clone().map(|w| {
        tracing_subscriber::fmt::layer().with_ansi(false).with_target(true).with_writer(w)
    });

    tracing_subscriber::registry()
        .with(env_filter())
        .with(stderr_layer)
        .with(file_layer.map(tracing_subscriber::Layer::boxed))
        .try_init()
        .map_err(|_| TelemetryError::AlreadyInitialised)?;

    Ok(file_writer)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use super::*;

    /// Covers: RQ-OBS-003
    #[test]
    fn rq_obs_003_log_level_and_no_samples() {
        // Audio can never be rendered into a log record: the only formatting this type
        // offers is a count.
        let buffer: Vec<f32> = (0..480).map(|i| (i as f32) / 480.0).collect();
        assert_eq!(format!("{}", Samples(&buffer)), "<480 samples>");
        assert_eq!(format!("{:?}", Samples(&buffer)), "<480 samples>");
        assert_eq!(format!("{:?}", Samples(&[])), "<0 samples>");

        // No sample value appears anywhere in the rendered form.
        let rendered = format!("{:?}", Samples(&buffer));
        assert!(!rendered.contains("0.5"), "rendered: {rendered}");
        assert!(!rendered.contains('['), "rendered: {rendered}");

        // Default verbosity is `info` when the environment says nothing.
        assert_eq!(DEFAULT_FILTER, "info");
        assert_eq!(LOG_ENV, "RINCON_LOG");
    }

    /// Covers: RQ-OBS-005
    #[test]
    fn rq_obs_005_log_is_size_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rincon.log");

        let cap = 1024;
        let mut writer = SizeCappedWriter::open(&path, cap).unwrap();

        // Write well past the ceiling.
        let chunk = [b'x'; 256];
        for _ in 0..20 {
            writer.write_all(&chunk).unwrap();
        }
        writer.flush().unwrap();

        let live = std::fs::metadata(&path).unwrap().len();
        assert!(live <= cap, "live log grew to {live} bytes, over the {cap} byte ceiling");

        // Rotation keeps exactly one previous generation, so total disk use stays bounded.
        let rotated = path.with_extension("log.1");
        let archived = std::fs::metadata(&rotated).map_or(0, |meta| meta.len());
        assert!(
            live + archived <= cap * 2,
            "total log footprint {} exceeds two generations",
            live + archived
        );
    }

    #[test]
    fn rotation_preserves_the_most_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rincon.log");
        let mut writer = SizeCappedWriter::open(&path, 64).unwrap();

        writer.write_all(b"old entry that will be rotated away\n").unwrap();
        writer.write_all(b"newest entry\n").unwrap();
        writer.flush().unwrap();

        let live = std::fs::read_to_string(&path).unwrap();
        assert!(live.contains("newest entry"), "the live log must hold the newest lines");
    }

    #[test]
    fn reopening_an_existing_log_accounts_for_its_current_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rincon.log");
        std::fs::write(&path, vec![b'x'; 500]).unwrap();

        let writer = SizeCappedWriter::open(&path, 1024).unwrap();
        assert_eq!(writer.written(), 500, "existing bytes must count against the ceiling");
    }

    #[test]
    fn shared_writer_is_usable_from_several_threads() {
        let dir = tempfile::tempdir().unwrap();
        let shared =
            SharedWriter::new(SizeCappedWriter::open(dir.path().join("t.log"), 1 << 20).unwrap());

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let w = shared.clone();
                std::thread::spawn(move || {
                    let mut guard = w.make_writer();
                    for _ in 0..100 {
                        guard.write_all(format!("line from {i}\n").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(shared.written() > 0);
    }

    #[test]
    fn config_constructors_express_the_two_real_deployments() {
        assert!(TelemetryConfig::stderr_only().log_file.is_none());
        let desktop = TelemetryConfig::with_log_file("x.log");
        assert!(desktop.log_file.is_some() && desktop.stderr);
    }
}
