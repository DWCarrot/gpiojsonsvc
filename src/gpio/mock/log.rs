//! Background append log for mock GPIO line-write XML dumps.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

enum LogCmd {
    Entry {
        timestamp_ms: u64,
        xml: String,
    },
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}

/// Background writer that appends timestamped chip XML dumps after line writes.
#[derive(Debug)]
pub struct XmlWriteLog {
    tx: mpsc::Sender<LogCmd>,
}

impl XmlWriteLog {
    /// Opens `path` in append mode and starts the `"mock-gpio-write-log"` thread.
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Arc<Self>> {
        let path = path.into();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("mock-gpio-write-log".to_owned())
            .spawn(move || write_loop(file, path, rx))
            .map_err(std::io::Error::other)?;
        Ok(Arc::new(Self { tx }))
    }

    /// Enqueues one dump entry. Channel errors only warn; they never fail the caller.
    pub fn enqueue(&self, timestamp_ms: u64, xml: String) {
        if let Err(error) = self.tx.send(LogCmd::Entry { timestamp_ms, xml }) {
            tracing::warn!(
                error = %error,
                "mock gpio write log enqueue failed"
            );
        }
    }

    /// Waits until the writer has processed all previously enqueued entries and flushed.
    #[cfg(test)]
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(LogCmd::Flush(ack_tx)).is_err() {
            return;
        }
        let _ = ack_rx.recv();
    }
}

fn write_loop(mut file: File, path: PathBuf, rx: mpsc::Receiver<LogCmd>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            LogCmd::Entry { timestamp_ms, xml } => {
                if let Err(error) = write_dump_block(&mut file, timestamp_ms, &xml) {
                    tracing::warn!(
                        error = %error,
                        path = %path.display(),
                        "mock gpio write log append failed"
                    );
                }
            }
            #[cfg(test)]
            LogCmd::Flush(ack) => {
                if let Err(error) = file.flush() {
                    tracing::warn!(
                        error = %error,
                        path = %path.display(),
                        "mock gpio write log flush failed"
                    );
                }
                let _ = ack.send(());
            }
        }
    }
    let _ = file.flush();
}

fn write_dump_block(writer: &mut impl Write, timestamp_ms: u64, xml: &str) -> std::io::Result<()> {
    writeln!(writer, "----")?;
    writeln!(writer, "timestamp: {timestamp_ms}")?;
    writeln!(writer)?;
    writer.write_all(xml.as_bytes())?;
    if !xml.ends_with('\n') {
        writeln!(writer)?;
    }
    writeln!(writer)?;
    Ok(())
}

/// Parses append-log dump blocks into `(timestamp_ms, xml)` pairs.
#[cfg(test)]
pub(crate) fn parse_write_log_blocks(content: &str) -> Vec<(u64, String)> {
    let mut blocks = Vec::new();
    for chunk in content.split("----\n") {
        if chunk.is_empty() {
            continue;
        }
        let rest = chunk
            .strip_prefix("timestamp: ")
            .unwrap_or_else(|| panic!("dump block missing timestamp line: {chunk:?}"));
        let (ts_str, after_ts) = rest
            .split_once('\n')
            .unwrap_or_else(|| panic!("dump block missing newline after timestamp: {chunk:?}"));
        let timestamp_ms: u64 = ts_str.parse().expect("timestamp millis");
        let xml = after_ts.trim_start_matches('\n').to_owned();
        blocks.push((timestamp_ms, xml));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::XmlWriteLog;
    use super::parse_write_log_blocks;
    use super::write_dump_block;
    use std::fs;
    use std::io::Cursor;
    use tempfile::NamedTempFile;

    #[test]
    fn dump_block_format_matches_plan() {
        let mut buffer = Cursor::new(Vec::new());
        write_dump_block(&mut buffer, 1710000000123, "<gpiochip id=\"gpiochip0\"/>\n")
            .expect("write dump block");

        let text = String::from_utf8(buffer.into_inner()).expect("utf8");
        assert_eq!(
            text,
            "----\ntimestamp: 1710000000123\n\n<gpiochip id=\"gpiochip0\"/>\n\n"
        );
    }

    #[test]
    fn enqueue_and_flush_appends_to_file() {
        let log_file = NamedTempFile::new().expect("temp log file");
        let path = log_file.path().to_path_buf();
        let log = XmlWriteLog::new(&path).expect("open write log");

        log.enqueue(100, "<gpiochip id=\"a\"/>\n".to_owned());
        log.enqueue(200, "<gpiochip id=\"b\"/>\n".to_owned());
        log.flush();

        let content = fs::read_to_string(&path).expect("read log");
        assert_eq!(
            content,
            "----\ntimestamp: 100\n\n<gpiochip id=\"a\"/>\n\n\
             ----\ntimestamp: 200\n\n<gpiochip id=\"b\"/>\n\n"
        );
    }
}
