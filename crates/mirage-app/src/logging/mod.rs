//! Logging setup (DESIGN.md §18): daily files, 7 days kept, 10 MiB per file,
//! and every formatted event passes through the redaction filter before it
//! reaches disk.

mod rolling;

use crate::domain::redact::redact_text;
pub use rolling::{MAX_FILE_BYTES, RollingWriter, newest_segment};
use std::io::{self, Write};
use std::path::Path;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

const DEFAULT_FILTER: &str = "info";

pub fn init(dir: &Path) -> anyhow::Result<WorkerGuard> {
    let writer = RollingWriter::open(dir.join("mirage.log"), MAX_FILE_BYTES)?;
    let (writer, guard) = tracing_appender::non_blocking(writer);

    let filter =
        EnvFilter::try_from_env("MIRAGE_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .with_writer(RedactingMakeWriter { inner: writer })
        .try_init()
        .map_err(|e| anyhow::anyhow!("install subscriber: {e}"))?;
    Ok(guard)
}

#[derive(Clone)]
struct RedactingMakeWriter {
    inner: NonBlocking,
}

impl<'a> MakeWriter<'a> for RedactingMakeWriter {
    type Writer = RedactingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.clone(),
            buf: Vec::new(),
        }
    }
}

/// Buffers one formatted event and redacts it as a whole on drop, so a secret
/// split across several `write` calls is still masked.
struct RedactingWriter {
    inner: NonBlocking,
    buf: Vec<u8>,
}

impl Write for RedactingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for RedactingWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.buf);
        // Logging must never panic or fail the caller; a dropped line is acceptable.
        let _ = self.inner.write_all(redact_text(&text).as_bytes());
    }
}
