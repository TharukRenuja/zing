use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

/// In-memory ring buffer of formatted log lines.
///
/// Used as the tracing writer while the TUI is active so downloader
/// warnings/errors are captured and shown in a panel instead of being
/// written to stderr (which would corrupt the alternate screen).
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
}

impl LogBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            max_lines,
        }
    }

    /// Copy of the captured lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.inner.lock().unwrap().iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let mut guard = self.inner.lock().unwrap();
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            if guard.len() >= self.max_lines {
                guard.pop_front();
            }
            guard.push_back(trimmed.to_string());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'w> MakeWriter<'w> for LogBuffer {
    type Writer = LogBuffer;

    fn make_writer(&'w self) -> Self::Writer {
        self.clone()
    }
}
