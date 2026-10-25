//! The Pi's own Wi-Fi for gigs, as set in the app (`/api/hotspot`).
//!
//! The server runs unprivileged, so it only saves the choice: `hotspot.conf`
//! in the service's state directory. The root `helix-bridge-hotspot` service
//! reads it at the next start-up and switches Wi-Fi (`deploy/hotspot.sh`), as
//! data only: the file is never run.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// What the Pi does with Wi-Fi at start-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Join the saved Wi-Fi only (the default).
    #[default]
    Off,
    /// A saved Wi-Fi if one is in range, else its own.
    Auto,
    /// Always its own.
    On,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Auto => "auto",
            Mode::On => "on",
        }
    }
}

pub const DEFAULT_NAME: &str = "Helix Bridge";

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    /// The Wi-Fi's name (SSID).
    pub name: String,
    /// WPA2 passphrase; empty until one is set.
    pub password: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mode: Mode::Off,
            name: DEFAULT_NAME.into(),
            password: String::new(),
        }
    }
}

/// WPA2's rule for a passphrase, and the most an SSID holds: printable ASCII,
/// so it reads the same on the iPad's keyboard and in the file.
fn printable(text: &str) -> bool {
    text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

impl Settings {
    /// Refuse what the Pi could not start (or the iPad not type).
    pub fn check(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 32 || !printable(&self.name) {
            return Err(
                "the Wi-Fi name must be 1 to 32 plain letters, digits or punctuation".into(),
            );
        }
        let len = self.password.len();
        if !printable(&self.password) || len > 63 || (len > 0 && len < 8) {
            return Err("the password must be 8 to 63 plain letters, digits or punctuation".into());
        }
        if self.mode != Mode::Off && len == 0 {
            return Err("set a password first: without one nobody could join".into());
        }
        Ok(())
    }

    fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            match line.split_once('=') {
                Some(("HOTSPOT", v)) => {
                    s.mode = match v {
                        "auto" => Mode::Auto,
                        "on" => Mode::On,
                        _ => Mode::Off,
                    }
                }
                Some(("NAME", v)) => s.name = v.to_owned(),
                Some(("PASSWORD", v)) => s.password = v.to_owned(),
                _ => {}
            }
        }
        s
    }

    fn render(&self) -> String {
        format!(
            "# Written by Helix Bridge (Settings, Own Wi-Fi for gigs). Read at start-up\n\
             # by helix-bridge-hotspot (deploy/hotspot.sh); never run as a script.\n\
             HOTSPOT={}\nNAME={}\nPASSWORD={}\n",
            self.mode.as_str(),
            self.name,
            self.password
        )
    }
}

/// Where the setting lives. One save at a time.
pub struct Store {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Store {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    /// `HELIX_HOTSPOT_FILE`, else `hotspot.conf` in systemd's
    /// `STATE_DIRECTORY` (`/var/lib/helix-bridge` on the Pi). `None` off the
    /// Pi: the app then hides the setting.
    pub fn from_env() -> Option<Self> {
        std::env::var_os("HELIX_HOTSPOT_FILE")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("STATE_DIRECTORY").map(|d| PathBuf::from(d).join("hotspot.conf"))
            })
            .map(Store::new)
    }

    /// The saved setting; off until the app saves one.
    pub fn load(&self) -> Settings {
        std::fs::read_to_string(&self.path)
            .map(|text| Settings::parse(&text))
            .unwrap_or_default()
    }

    /// Replace the file whole (a reader never sees half of it), readable by
    /// this user and root only: it holds the password.
    pub fn save(&self, settings: &Settings) -> std::io::Result<()> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = self.path.with_extension("conf.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(settings.render().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("helix-hotspot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(dir.join("hotspot.conf"));
        assert_eq!(store.load(), Settings::default(), "no file: off");

        let s = Settings {
            mode: Mode::Auto,
            name: "Anna's rig = loud".into(),
            password: " pass=word#1 ".into(),
        };
        store.save(&s).unwrap();
        assert_eq!(store.load(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("hotspot.conf"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_mode_reads_as_off() {
        assert_eq!(Settings::parse("HOTSPOT=yes\n").mode, Mode::Off);
        assert_eq!(Settings::parse("HOTSPOT=on\n").mode, Mode::On);
    }

    #[test]
    fn checks() {
        let ok = |mode, name: &str, password: &str| {
            Settings {
                mode,
                name: name.into(),
                password: password.into(),
            }
            .check()
        };
        assert!(ok(Mode::Off, "Helix Bridge", "").is_ok());
        assert!(ok(Mode::Auto, "Helix Bridge", "12345678").is_ok());
        assert!(ok(Mode::On, &"x".repeat(32), &"y".repeat(63)).is_ok());

        assert!(ok(Mode::Auto, "Helix Bridge", "").is_err(), "no password");
        assert!(ok(Mode::Off, "Helix Bridge", "short").is_err());
        assert!(ok(Mode::On, "Helix Bridge", &"y".repeat(64)).is_err());
        assert!(ok(Mode::On, "", "12345678").is_err());
        assert!(ok(Mode::On, &"x".repeat(33), "12345678").is_err());
        assert!(ok(Mode::On, "Café", "12345678").is_err());
        assert!(ok(Mode::On, "Line\nPASSWORD=x", "12345678").is_err());
        assert!(ok(Mode::On, "Helix", "1234567\n8").is_err());
    }
}
