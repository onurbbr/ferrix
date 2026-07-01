//! Minimal trace output abstraction for VM instruction logging.

use std::fmt::Write;

/// Receives formatted trace lines emitted while the VM executes instructions.
pub trait TraceWriter {
    /// Writes one already-formatted trace line.
    fn write_trace_line(&mut self, line: &str);
}

impl TraceWriter for String {
    fn write_trace_line(&mut self, line: &str) {
        writeln!(self, "{line}").expect("writing to String cannot fail");
    }
}
