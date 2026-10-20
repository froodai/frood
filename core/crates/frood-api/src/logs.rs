//! The core's recent log lines, kept in memory for the admin panel (C2). The server's log
//! writer sends every line to stdout as before and also here.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

/// About a day of an idle server, a few hours of a busy one.
const LINES: usize = 5000;

struct Ring {
    next: u64,
    lines: VecDeque<(u64, String)>,
}

static RING: OnceLock<Mutex<Ring>> = OnceLock::new();

fn ring() -> &'static Mutex<Ring> {
    RING.get_or_init(|| {
        Mutex::new(Ring {
            next: 1,
            lines: VecDeque::with_capacity(LINES),
        })
    })
}

fn keep(text: &str) {
    let Ok(mut ring) = ring().lock() else {
        return;
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if ring.lines.len() == LINES {
            ring.lines.pop_front();
        }
        let seq = ring.next;
        ring.next += 1;
        ring.lines.push_back((seq, line.to_owned()));
    }
}

/// Lines after `after`, oldest first, at most `limit`; JSON lines come back parsed.
pub fn after(after: u64, limit: usize) -> Vec<(u64, Value)> {
    let Ok(ring) = ring().lock() else {
        return Vec::new();
    };
    ring.lines
        .iter()
        .filter(|(seq, _)| *seq > after)
        .take(limit)
        .map(|(seq, line)| {
            (
                *seq,
                serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.clone())),
            )
        })
        .collect()
}

/// A `tracing_subscriber` writer: stdout, and the ring.
pub struct Tee;

pub struct TeeWriter(Vec<u8>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Tee {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> TeeWriter {
        TeeWriter(Vec::new())
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for TeeWriter {
    fn drop(&mut self) {
        let _ = std::io::stdout().write_all(&self.0);
        keep(&String::from_utf8_lossy(&self.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_kept_in_order() {
        let start = after(0, usize::MAX).last().map_or(0, |(seq, _)| *seq);
        {
            let mut w = TeeWriter(Vec::new());
            w.write_all(b"{\"level\":\"INFO\",\"fields\":{\"message\":\"one\"}}\n")
                .unwrap();
        }
        {
            let mut w = TeeWriter(Vec::new());
            w.write_all(b"plain line\n").unwrap();
        }
        let got = after(start, 10);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1["fields"]["message"], "one");
        assert_eq!(got[1].1, "plain line");
        assert!(after(got[1].0, 10).is_empty());
    }
}
