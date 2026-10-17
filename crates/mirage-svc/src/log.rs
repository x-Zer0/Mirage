//! The same bounded writer as the app; never log config bodies or secrets.
use mirage_ipc::rolling::{MAX_FILE_BYTES, RollingWriter};
use std::{path::PathBuf, sync::OnceLock};
static SINK: OnceLock<RollingWriter> = OnceLock::new();

pub fn init(path: PathBuf) {
    match RollingWriter::open(path, MAX_FILE_BYTES) {
        Ok(writer) => {
            let _ = SINK.set(writer);
        }
        Err(_) => eprintln!("mirage-svc: cannot open log"),
    }
}
pub fn info(message: &str) {
    write("INFO", message);
}
pub fn warn(message: &str) {
    write("WARN", message);
}
pub fn error(message: &str) {
    write("ERROR", message);
}
fn write(level: &str, message: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = format!("{now} {level} {message}\n");
    if let Some(sink) = SINK.get() {
        let _ = sink.append(line.as_bytes());
    }
}
