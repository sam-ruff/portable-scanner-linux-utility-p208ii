//! Watches the hardware start button while the desktop app is closed.

use std::cell::RefCell;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use log::{debug, info, warn};

use crate::cdb::{self, datatype};
use crate::channel::{Request, ScsiChannel};
use crate::error::ScanError;
use crate::session::open_usb_channel;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const PANEL_LEN: usize = 8;

fn lock_path() -> Result<PathBuf, ScanError> {
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .map(|dir| dir.join("p208ii/application.lock"))
        .ok_or_else(|| ScanError::Io("could not find a directory for the scanner lock".into()))
}

fn try_lock(path: &Path) -> Result<Option<File>, ScanError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err.into()),
    }
}

/// Held for the lifetime of an app or CLI scan, so the button watcher stays out of USB.
pub fn lock_application() -> Result<File, ScanError> {
    let path = lock_path()?;
    for _ in 0..20 {
        if let Some(lock) = try_lock(&path)? {
            return Ok(lock);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(ScanError::Io(
        "Receipt Scanner is already open or scanning".into(),
    ))
}

fn read_start_button(channel: &impl ScsiChannel) -> Result<bool, ScanError> {
    let reply = channel.execute(
        Request::new(cdb::read(datatype::PANEL, PANEL_LEN))
            .reading(PANEL_LEN)
            .with_timeout(Duration::from_secs(1)),
    )?;
    if reply.data.len() != PANEL_LEN {
        return Err(ScanError::Protocol("panel reply too short".into()));
    }
    Ok(reply.data[0] & 0x80 != 0)
}

#[cfg_attr(test, mockall::automock)]
trait ButtonActions {
    fn pressed(&self) -> Result<bool, ScanError>;
    fn launch(&self) -> Result<(), ScanError>;
}

struct DesktopActions {
    executable: PathBuf,
    child: RefCell<Option<Child>>,
}

impl ButtonActions for DesktopActions {
    fn pressed(&self) -> Result<bool, ScanError> {
        // Opening Scanner would reset the panel and lose a latched button press.
        read_start_button(&open_usb_channel()?)
    }

    fn launch(&self) -> Result<(), ScanError> {
        let mut child = self.child.borrow_mut();
        if let Some(process) = child.as_mut()
            && process.try_wait()?.is_none()
        {
            return Ok(());
        }
        *child = Some(Command::new(&self.executable).spawn()?);
        info!("Scanner button pressed; opened Receipt Scanner");
        Ok(())
    }
}

#[derive(Default)]
struct ButtonMonitor {
    was_pressed: bool,
}

impl ButtonMonitor {
    fn poll(&mut self, actions: &impl ButtonActions) -> Result<(), ScanError> {
        let pressed = actions.pressed()?;
        let launch = pressed && !self.was_pressed;
        self.was_pressed = pressed;
        if launch {
            actions.launch()?;
        }
        Ok(())
    }
}

pub fn watch() -> Result<(), ScanError> {
    let path = lock_path()?;
    let actions = DesktopActions {
        executable: std::env::current_exe()?,
        child: RefCell::new(None),
    };
    let mut monitor = ButtonMonitor::default();
    let mut last_error = None;
    info!("Watching the scanner button");
    loop {
        if let Some(_lock) = try_lock(&path)? {
            match monitor.poll(&actions) {
                Ok(()) => last_error = None,
                Err(err) => {
                    let message = err.to_string();
                    if last_error.as_ref() != Some(&message) {
                        if matches!(err, ScanError::NotFound(_)) {
                            debug!("{message}");
                        } else {
                            warn!("{message}");
                        }
                        last_error = Some(message);
                    }
                    monitor.was_pressed = false;
                }
            }
        }
        if let Some(child) = actions.child.borrow_mut().as_mut() {
            let _ = child.try_wait();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{MockScsiChannel, Reply};

    #[test]
    fn panel_read_only_uses_start_bit_and_does_not_reset_panel() {
        for (byte, expected) in [(0, false), (0x40, false), (0x80, true), (0xc4, true)] {
            let mut channel = MockScsiChannel::new();
            channel
                .expect_execute()
                .withf(|r| r.cdb == cdb::read(datatype::PANEL, 8) && r.read_len == 8)
                .times(1)
                .return_once(move |_| {
                    let mut data = vec![0; 8];
                    data[0] = byte;
                    Ok(Reply { data, short: false })
                });
            assert_eq!(read_start_button(&channel), Ok(expected));
        }
    }

    #[test]
    fn short_panel_reply_is_rejected() {
        let mut channel = MockScsiChannel::new();
        channel.expect_execute().returning(|_| Ok(Reply::default()));
        assert!(matches!(
            read_start_button(&channel),
            Err(ScanError::Protocol(_))
        ));
    }

    #[test]
    fn held_button_launches_once_and_release_allows_another_press() {
        let mut actions = MockButtonActions::new();
        let mut samples = [false, true, true, false, true].into_iter();
        actions
            .expect_pressed()
            .times(5)
            .returning(move || Ok(samples.next().expect("one sample per poll")));
        actions.expect_launch().times(2).returning(|| Ok(()));
        let mut monitor = ButtonMonitor::default();
        for _ in 0..5 {
            monitor.poll(&actions).expect("poll succeeds");
        }
    }

    #[test]
    fn read_failure_never_launches() {
        let mut actions = MockButtonActions::new();
        actions.expect_pressed().returning(|| Err(ScanError::Busy));
        actions.expect_launch().never();
        assert_eq!(
            ButtonMonitor::default().poll(&actions),
            Err(ScanError::Busy)
        );
    }

    #[test]
    fn launch_failure_is_reported() {
        let mut actions = MockButtonActions::new();
        actions.expect_pressed().returning(|| Ok(true));
        actions
            .expect_launch()
            .times(1)
            .returning(|| Err(ScanError::Io("launch failed".into())));
        assert!(matches!(
            ButtonMonitor::default().poll(&actions),
            Err(ScanError::Io(_))
        ));
    }

    #[test]
    fn application_lock_excludes_watcher_until_released() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("application.lock");
        let app = try_lock(&path).expect("lock file").expect("app lock");
        assert!(try_lock(&path).expect("lock file").is_none());
        drop(app);
        assert!(try_lock(&path).expect("lock file").is_some());
    }
}
