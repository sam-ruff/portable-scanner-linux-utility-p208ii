//! The scan loop the UI and command line drive: wait for a receipt, scan it,
//! save it, repeat. It talks to its caller only through channels.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use log::{error, info};

use crate::channel::{ScsiChannel, UsbScsiChannel};
use crate::crop::smart_crop;
use crate::encode::FileFormat;
use crate::error::ScanError;
use crate::image::Image;
use crate::output::ScanFolder;
use crate::params::ScanSettings;
use crate::scanner::Scanner;
use crate::simulator::{PaperSupply, SimulatedScanner};
use crate::usb::{NusbTransport, find_scanner};

const USB_RECOVERY_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connected {
        model: String,
        firmware: String,
    },
    WaitingForPaper,
    Preparing,
    Scanning,
    Saved {
        paths: Vec<PathBuf>,
        images: Vec<Image>,
    },
    Finished {
        saved: usize,
    },
    Failed {
        message: String,
    },
}

pub trait EventSink {
    fn emit(&self, event: Event);
}

impl EventSink for Sender<Event> {
    fn emit(&self, event: Event) {
        // A closed receiver means nobody is listening any more
        let _ = self.send(event);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOptions {
    pub settings: ScanSettings,
    /// Keep waiting for more receipts after the feeder runs dry.
    pub continuous: bool,
    pub poll_interval: Duration,
    pub format: FileFormat,
    /// Trim the scanner background from around each receipt.
    pub smart_crop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Usb,
    Simulated {
        supply: PaperSupply,
        read_delay: Duration,
    },
}

pub fn open_scanner(backend: &Backend) -> Result<Scanner<Box<dyn ScsiChannel>>, ScanError> {
    let channel: Box<dyn ScsiChannel> = match backend {
        Backend::Usb => Box::new(open_usb_channel()?),
        Backend::Simulated { supply, read_delay } => {
            info!("Using the simulated scanner");
            Box::new(SimulatedScanner::new(*supply, *read_delay))
        }
    };
    Scanner::open(channel)
}

pub fn open_usb_channel() -> Result<UsbScsiChannel<NusbTransport>, ScanError> {
    let found = find_scanner()?;
    let transport = NusbTransport::open(&found)?;
    Ok(UsbScsiChannel::new(transport, USB_RECOVERY_DELAY))
}

pub fn feed_paper(backend: &Backend) -> Result<(), ScanError> {
    let scanner = open_scanner(backend)?;
    info!("Feeding paper through without scanning");
    scanner.feed_paper()?;
    info!("Paper feed complete");
    Ok(())
}

fn stop_requested(commands: &Receiver<Command>) -> bool {
    matches!(
        commands.try_recv(),
        Ok(Command::Stop) | Err(TryRecvError::Disconnected)
    )
}

/// Sleeps for `interval` unless told to stop first. Returns true to stop.
fn pause(commands: &Receiver<Command>, interval: Duration) -> bool {
    !matches!(
        commands.recv_timeout(interval),
        Err(RecvTimeoutError::Timeout)
    )
}

/// Polls the paper sensor. Returns false if asked to stop while waiting.
fn wait_for_paper(
    scanner: &Scanner<impl ScsiChannel>,
    options: &SessionOptions,
    commands: &Receiver<Command>,
    events: &impl EventSink,
) -> Result<bool, ScanError> {
    let mut announced = false;
    loop {
        if stop_requested(commands) {
            return Ok(false);
        }
        if scanner.paper_loaded()? {
            return Ok(true);
        }
        if !announced {
            info!("Waiting for paper");
            events.emit(Event::WaitingForPaper);
            announced = true;
        }
        if pause(commands, options.poll_interval) {
            return Ok(false);
        }
    }
}

/// Scans receipts into `folder` until stopped, or until the feeder empties
/// when not continuous. Returns the number of sheets saved.
pub fn run_session(
    scanner: &mut Scanner<impl ScsiChannel>,
    folder: &mut ScanFolder,
    options: &SessionOptions,
    commands: &Receiver<Command>,
    events: &impl EventSink,
) -> Result<usize, ScanError> {
    let mut saved = 0;
    loop {
        if !wait_for_paper(scanner, options, commands, events)? {
            return Ok(saved);
        }
        events.emit(Event::Preparing);
        let mut batch = scanner.start_batch(&options.settings)?;
        loop {
            if stop_requested(commands) {
                batch.cancel();
                return Ok(saved);
            }
            events.emit(Event::Scanning);
            let Some(images) = batch.next_page()? else {
                break;
            };
            if images.is_empty() {
                continue;
            }
            let images = if options.smart_crop {
                images.into_iter().map(smart_crop).collect()
            } else {
                images
            };
            let paths = folder.save(&images)?;
            let names: Vec<_> = paths
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy())
                .collect();
            info!("Saved {}", names.join(" and "));
            saved += 1;
            events.emit(Event::Saved { paths, images });
        }
        let scanned = batch.pages();
        drop(batch);

        if !options.continuous {
            return Ok(saved);
        }
        // The sensor saw paper but none fed, so give it a moment rather than spin
        if scanned == 0 && pause(commands, options.poll_interval) {
            return Ok(saved);
        }
    }
}

fn run(
    backend: &Backend,
    output_dir: PathBuf,
    options: &SessionOptions,
    commands: &Receiver<Command>,
    events: &impl EventSink,
) -> Result<usize, ScanError> {
    let mut folder = ScanFolder::open(output_dir, options.format)?;
    let mut scanner = open_scanner(backend)?;
    let info = scanner.info();
    events.emit(Event::Connected {
        model: info.model.clone(),
        firmware: info.firmware.clone(),
    });
    run_session(&mut scanner, &mut folder, options, commands, events)
}

/// Entry point for a worker thread. Always finishes with `Finished` or `Failed`.
pub fn run_worker(
    backend: Backend,
    output_dir: PathBuf,
    options: SessionOptions,
    commands: Receiver<Command>,
    events: impl EventSink,
) {
    match run(&backend, output_dir, &options, &commands, &events) {
        Ok(saved) => events.emit(Event::Finished { saved }),
        Err(err) => {
            error!("{err}");
            events.emit(Event::Failed {
                message: err.to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::params::ColourMode;

    fn options(continuous: bool) -> SessionOptions {
        SessionOptions {
            settings: ScanSettings {
                dpi: 150,
                page_width_mm: Some(60),
                mode: ColourMode::Grey,
                ..ScanSettings::default()
            },
            continuous,
            poll_interval: Duration::from_millis(5),
            format: FileFormat::Png,
            smart_crop: false,
        }
    }

    fn simulated(supply: PaperSupply) -> Backend {
        Backend::Simulated {
            supply,
            read_delay: Duration::ZERO,
        }
    }

    fn collect(events: &Receiver<Event>) -> Vec<Event> {
        events.try_iter().collect()
    }

    #[test]
    fn saves_each_sheet_then_finishes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (_command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        run_worker(
            simulated(PaperSupply::Sheets(3)),
            dir.path().to_path_buf(),
            options(false),
            command_rx,
            event_tx,
        );

        let events = collect(&event_rx);
        let saved: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Saved { paths, .. } => Some(paths[0].clone()),
                _ => None,
            })
            .collect();
        assert_eq!(saved.len(), 3);
        assert!(saved.iter().all(|p| p.is_file()));
        assert_eq!(
            events.first(),
            Some(&Event::Connected {
                model: "P-208II".into(),
                firmware: "SIM".into()
            })
        );
        assert_eq!(events.last(), Some(&Event::Finished { saved: 3 }));
    }

    #[test]
    fn smart_crop_trims_full_width_scans_to_the_receipt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (_command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let mut options = options(false);
        options.settings.page_width_mm = None;
        options.smart_crop = true;

        run_worker(
            simulated(PaperSupply::Sheets(1)),
            dir.path().to_path_buf(),
            options,
            command_rx,
            event_tx,
        );

        let path = collect(&event_rx)
            .into_iter()
            .find_map(|e| match e {
                Event::Saved { paths, .. } => paths.into_iter().next(),
                _ => None,
            })
            .expect("a scan was saved");
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(path).expect("file exists"),
        ));
        let info = decoder.read_info().expect("valid png").info().clone();
        // The first simulated receipt is 80 mm wide, about 472 px at 150 dpi
        let expected = 80 * 150 * 10 / 254;
        assert!(
            (info.width as usize).abs_diff(expected) <= 4,
            "cropped to {} px",
            info.width
        );
    }

    #[test]
    fn stop_while_waiting_ends_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = std::thread::spawn({
            let output = dir.path().to_path_buf();
            move || {
                run_worker(
                    simulated(PaperSupply::Sheets(0)),
                    output,
                    options(true),
                    command_rx,
                    event_tx,
                )
            }
        });
        let first_wait = event_rx.iter().find(|e| *e == Event::WaitingForPaper);
        assert!(first_wait.is_some());
        command_tx.send(Command::Stop).expect("worker listening");
        worker.join().expect("worker exits cleanly");

        assert_eq!(
            collect(&event_rx).last(),
            Some(&Event::Finished { saved: 0 })
        );
    }

    #[test]
    fn dropped_command_channel_stops_worker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel();
        drop(command_tx);

        run_worker(
            simulated(PaperSupply::Every(Duration::ZERO)),
            dir.path().to_path_buf(),
            options(true),
            command_rx,
            event_tx,
        );
        assert_eq!(
            collect(&event_rx).last(),
            Some(&Event::Finished { saved: 0 })
        );
    }

    #[test]
    fn continuous_mode_keeps_scanning_new_receipts() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = std::thread::spawn({
            let output = dir.path().to_path_buf();
            move || {
                run_worker(
                    simulated(PaperSupply::Every(Duration::from_millis(20))),
                    output,
                    options(true),
                    command_rx,
                    event_tx,
                )
            }
        });
        let saves = event_rx
            .iter()
            .filter(|e| matches!(e, Event::Saved { .. }))
            .take(3)
            .count();
        command_tx.send(Command::Stop).expect("worker listening");
        worker.join().expect("worker exits cleanly");

        assert_eq!(saves, 3);
        match collect(&event_rx).last() {
            Some(Event::Finished { saved }) => assert!(*saved >= 3),
            other => panic!("expected a clean finish, got {other:?}"),
        }
    }

    #[test]
    fn unusable_output_folder_reports_failure() {
        let (_command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let file = tempfile::NamedTempFile::new().expect("temp file");

        // A file where the folder should be makes setup fail before any USB access
        run_worker(
            Backend::Usb,
            file.path().to_path_buf(),
            options(false),
            command_rx,
            event_tx,
        );
        assert!(matches!(
            collect(&event_rx).last(),
            Some(Event::Failed { .. })
        ));
    }
}
