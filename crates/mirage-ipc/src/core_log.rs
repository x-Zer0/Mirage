//! Removes access events before either app or service writes core output to disk.
//! Incremental framing handles split pipe chunks and bounds oversized lines.
#[derive(Default)]
pub struct DiagnosticLines {
    pending: Vec<u8>,
    oversized: bool,
}
impl DiagnosticLines {
    pub fn push(&mut self, bytes: &[u8], mut write: impl FnMut(&[u8])) {
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            if !self.oversized {
                self.pending.extend_from_slice(part);
                if self.pending.len() > 64 * 1024 {
                    self.pending.clear();
                    self.oversized = true;
                }
            }
            if part.ends_with(b"\n") {
                if !self.oversized && !is_connection_event(&self.pending) {
                    write(&self.pending);
                }
                self.pending.clear();
                self.oversized = false;
            }
        }
    }
    pub fn finish(&mut self, mut write: impl FnMut(&[u8])) {
        if !self.oversized && !is_connection_event(&self.pending) {
            write(&self.pending);
        }
        self.pending.clear();
    }
}
fn is_connection_event(line: &[u8]) -> bool {
    let text = String::from_utf8_lossy(line);
    (text.contains("[TCP]") || text.contains("[UDP]")) && text.contains("-->")
}
