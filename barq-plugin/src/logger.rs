//! Forward `log` records to Core Lightning as `log` notifications.
//!
//! A plugin talks to `lightningd` over stdout, so the only way to get
//! messages into the node log is to emit JSON-RPC `log` notifications.
//! Each record is written with a single locked `write_all`, so records
//! coming from the background sync thread never interleave with RPC
//! responses.
use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct ClnLogger;

impl Log for ClnLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // LDK is extremely chatty at debug/trace; only surface its
        // informational messages.
        let target = metadata.target();
        if target.starts_with("ldk") || target.starts_with("lightning") {
            metadata.level() <= Level::Info
        } else {
            metadata.level() <= Level::Debug
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug | Level::Trace => "debug",
        };
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "log",
            "params": {
                "level": level,
                "message": format!("{}: {}", record.target(), record.args()),
            }
        });
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(notification.to_string().as_bytes());
        let _ = out.write_all(b"\n\n");
        let _ = out.flush();
    }

    fn flush(&self) {}
}

static LOGGER: ClnLogger = ClnLogger;

pub fn init() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Debug);
    }
}
