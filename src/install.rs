//! Per-user desktop installation and scanner-button service.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::ScanError;

const SERVICE: &str = "p208ii-button.service";

struct Locations {
    binary: PathBuf,
    desktop: PathBuf,
    service: PathBuf,
    rule: PathBuf,
}

impl Locations {
    fn new(home: &Path, config: &Path, data: &Path) -> Self {
        Self {
            binary: home.join(".local/bin/p208ii"),
            desktop: data.join("applications/p208ii.desktop"),
            service: config.join("systemd/user").join(SERVICE),
            rule: data.join("p208ii/60-canon-p208ii.rules"),
        }
    }

    fn current() -> Result<Self, ScanError> {
        let required = |path: Option<PathBuf>| {
            path.ok_or_else(|| ScanError::Io("could not find your home directories".into()))
        };
        Ok(Self::new(
            &required(dirs::home_dir())?,
            &required(dirs::config_dir())?,
            &required(dirs::data_local_dir())?,
        ))
    }

    fn write(&self, executable: &Path) -> Result<(), ScanError> {
        for path in [&self.binary, &self.desktop, &self.service, &self.rule] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        let parent = self
            .binary
            .parent()
            .ok_or_else(|| ScanError::Io("invalid install path".into()))?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        fs::copy(executable, temporary.path())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.binary)
            .map_err(|err| ScanError::Io(err.to_string()))?;
        fs::write(&self.desktop, desktop_entry(&self.binary))?;
        fs::write(
            &self.service,
            include_str!("../packaging/p208ii-button.service"),
        )?;
        fs::write(
            &self.rule,
            include_str!("../packaging/60-canon-p208ii.rules"),
        )?;
        Ok(())
    }
}

fn desktop_entry(binary: &Path) -> String {
    let executable = binary
        .to_string_lossy()
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\"")
        .replace('`', "\\\\`")
        .replace('$', "\\\\$")
        .replace('%', "%%")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!(
        "[Desktop Entry]\nType=Application\nName=Receipt Scanner\n\
         Comment=Scan receipts with a Canon P-208II\nExec=\"{executable}\"\n\
         Icon=scanner\nTerminal=false\nCategories=Office;Graphics;Scanning;\n"
    )
}

fn systemctl(arguments: &[&str]) -> Result<(), ScanError> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .status()?;
    if !status.success() {
        return Err(ScanError::Io(format!(
            "systemctl --user {} failed",
            arguments.join(" ")
        )));
    }
    Ok(())
}

pub fn install() -> Result<(), ScanError> {
    let locations = Locations::current()?;
    locations.write(&std::env::current_exe()?)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", SERVICE])?;
    systemctl(&["restart", SERVICE])?;
    println!(
        "Installed Receipt Scanner to {}",
        locations.binary.display()
    );
    println!("The blue scanner button now opens the app while you are logged into your desktop.");
    Ok(())
}

pub fn uninstall() -> Result<(), ScanError> {
    let locations = Locations::current()?;
    systemctl(&["disable", "--now", SERVICE])?;
    for path in [
        &locations.service,
        &locations.desktop,
        &locations.binary,
        &locations.rule,
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    systemctl(&["daemon-reload"])?;
    println!(
        "Removed Receipt Scanner and its button listener. Your scans and preferences are kept."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn install_and_upgrade_replace_binary_and_keep_other_files() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let root = dir.path();
        let locations = Locations::new(root, &root.join("config"), &root.join("data"));
        let source = root.join("source");
        fs::write(&source, "first").expect("source");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).expect("executable");
        locations.write(&source).expect("install");
        let running = fs::File::open(&locations.binary).expect("installed binary");
        fs::write(&source, "second").expect("new binary");
        locations.write(&source).expect("upgrade");
        assert_eq!(
            fs::read_to_string(&locations.binary).expect("contents"),
            "second"
        );
        assert_eq!(running.metadata().expect("old file").len(), 5);
        assert_ne!(
            fs::metadata(&locations.binary)
                .expect("binary")
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert!(
            fs::read_to_string(&locations.service)
                .expect("service")
                .contains("watch-button")
        );
        assert!(locations.desktop.is_file());
        assert!(locations.rule.is_file());
    }

    #[test]
    fn desktop_exec_quotes_spaces_and_escapes_field_codes() {
        let entry = desktop_entry(Path::new("/home/A B/100%/p208ii"));
        assert!(entry.contains("Exec=\"/home/A B/100%%/p208ii\""));
        assert!(!entry.contains("Terminal=true"));
    }
}
