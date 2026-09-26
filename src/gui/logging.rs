use std::time::Instant;

use iced::futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use log::{Level, LevelFilter, Log, Metadata, Record};

/// Sends log lines to the UI over a channel and still prints to stderr.
struct UiLogger {
    stderr: env_logger::Logger,
    lines: UnboundedSender<String>,
    started: Instant,
}

fn wanted_by_ui(metadata: &Metadata) -> bool {
    if metadata.target().starts_with(env!("CARGO_CRATE_NAME")) {
        return metadata.level() <= Level::Debug;
    }
    metadata.level() <= Level::Warn
}

pub fn format_line(elapsed_secs: f64, level: Level, message: &str) -> String {
    format!("{elapsed_secs:>8.1}s {level:<5} {message}")
}

impl Log for UiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        wanted_by_ui(metadata) || self.stderr.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.stderr.matches(record) {
            self.stderr.log(record);
        }
        if wanted_by_ui(record.metadata()) {
            let line = format_line(
                self.started.elapsed().as_secs_f64(),
                record.level(),
                &record.args().to_string(),
            );
            // The UI may have closed already
            let _ = self.lines.unbounded_send(line);
        }
    }

    fn flush(&self) {
        self.stderr.flush();
    }
}

/// Installs the global logger and returns the stream of lines for the UI.
pub fn init() -> UnboundedReceiver<String> {
    let (lines, receiver) = unbounded();
    let stderr =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    let max_level = stderr.filter().max(LevelFilter::Debug);
    let logger = UiLogger {
        stderr,
        lines,
        started: Instant::now(),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(max_level);
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_aligned() {
        assert_eq!(
            format_line(3.34, Level::Info, "Saved scan-0001.png"),
            "     3.3s INFO  Saved scan-0001.png"
        );
    }

    #[test]
    fn ui_shows_driver_debug_but_not_library_noise() {
        let ours = Metadata::builder()
            .target(concat!(env!("CARGO_CRATE_NAME"), "::scanner"))
            .level(Level::Debug)
            .build();
        let noisy = Metadata::builder()
            .target("wgpu_core")
            .level(Level::Info)
            .build();
        let warning = Metadata::builder()
            .target("wgpu_core")
            .level(Level::Warn)
            .build();
        assert!(wanted_by_ui(&ours));
        assert!(!wanted_by_ui(&noisy));
        assert!(wanted_by_ui(&warning));
    }
}
