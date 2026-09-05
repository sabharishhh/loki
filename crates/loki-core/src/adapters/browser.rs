//! Launching the browser rung 2 drives (§12.3).
//!
//! **What is here and what is not.** This module finds an installed Chromium, launches it against a
//! Loki-owned profile with every request pointed at the exit, and kills it when the session drops.
//! It does not speak CDP: driving pages is §12.10's work and needs a protocol client, which is a
//! dependency decision rather than a line of code.
//!
//! **Nothing here opens a socket**, which is why it sits outside the transport rule in
//! `tests/rings.rs`. It starts a process and passes it an address. The browser's own traffic is the
//! thing being governed, and it is governed by the exit it is pointed at.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crate::ports::egress::Delegated;

/// A Chromium-family browser on this machine.
///
/// **Family, not Chrome.** What rung 2 needs is CDP, and every Chromium fork speaks it. Naming one
/// vendor would exclude somebody who deliberately runs another and buy nothing. Verified against
/// Brave 152, which answers `/json/version` as `Chrome/152.0.7977.83` on protocol 1.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chromium {
    /// What to call it when the ladder reports which browser answered.
    pub name: &'static str,
    pub binary: PathBuf,
}

impl Chromium {
    /// The first installed browser, in preference order.
    ///
    /// Brave leads on purpose rather than alphabetically: it sends a Chrome user agent, has a very
    /// large real user base, and ships fingerprint randomisation and ad blocking of its own, so it
    /// is a better default than Chrome rather than a fallback from it.
    #[must_use]
    pub fn detect() -> Option<Self> {
        Self::detect_under(Path::new("/Applications"))
    }

    /// `detect`, rooted somewhere else. For tests, which must not depend on what is installed.
    #[must_use]
    pub fn detect_under(applications: &Path) -> Option<Self> {
        Self::candidates()
            .into_iter()
            .map(|(name, app, binary)| Self {
                name,
                binary: applications.join(app).join("Contents/MacOS").join(binary),
            })
            .find(|found| found.binary.is_file())
    }

    /// Name, application bundle, and the executable inside it.
    #[must_use]
    pub const fn candidates() -> [(&'static str, &'static str, &'static str); 7] {
        [
            ("Brave", "Brave Browser.app", "Brave Browser"),
            ("Chrome", "Google Chrome.app", "Google Chrome"),
            ("Edge", "Microsoft Edge.app", "Microsoft Edge"),
            ("Vivaldi", "Vivaldi.app", "Vivaldi"),
            ("Chromium", "Chromium.app", "Chromium"),
            ("Arc", "Arc.app", "Arc"),
            ("Opera", "Opera.app", "Opera"),
        ]
    }
}

/// Flags that keep the byte accounting honest (§21.7).
///
/// **These are specification, not tuning.** Measured on Brave 152: an ordinary launch made 31
/// requests across 8 hosts to load one page, of which 2 were the page. The rest was the updater,
/// component fetches, variations and telemetry, and all of it arrives at the exit as Loki's egress.
/// With these set the same load made 2 requests. Whatever survives is denied at the exit and
/// recorded, which is the honest outcome rather than a silent one.
///
/// A browser's own telemetry is also a privacy fact and not only an accounting one: the user did
/// not ask their assistant to tell their browser vendor they are running.
const QUIET: [&str; 8] = [
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--disable-domain-reliability",
    "--disable-breakpad",
    "--no-pings",
    "--metrics-recording-only",
    "--no-default-browser-check",
];

/// Flags every launch needs regardless of accounting.
const BASE: [&str; 3] = [
    "--no-first-run",
    "--homepage=about:blank",
    "--password-store=basic",
];

/// A running browser.
///
/// **It cannot be constructed without an exit.** §21.7 requires every outbound socket to be opened
/// by one exit, and a browser opens its own. Holding the exit is what makes this legal, so `open`
/// takes one and the session keeps it alive for as long as the browser runs. There is no
/// constructor that takes a bare binary path, which is the difference between a rule somebody
/// remembers and one the compiler enforces.
#[derive(Debug)]
pub struct Session {
    child: Child,
    port: u16,
    /// Kept so the exit outlives the browser pointed at it. Never read.
    _exit: Arc<Delegated>,
}

impl Session {
    /// Launches the browser, pointed at the exit.
    ///
    /// `profile` is Loki's own directory and never the user's. A browser already running cannot be
    /// given these flags, and attaching to a live profile would hand Loki the user's cookies,
    /// history and logged-in sessions for a job that needs none of them (§12.3).
    ///
    /// # Errors
    /// Fails if the browser cannot be started.
    pub fn open(
        browser: &Chromium,
        exit: Arc<Delegated>,
        profile: &Path,
        port: u16,
    ) -> Result<Self, BrowserError> {
        let child = Command::new(&browser.binary)
            .args(BASE)
            .args(QUIET)
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--proxy-server={}", exit.proxy_url()))
            // Nothing bypasses the exit. Loopback is exempt by default and stays that way, which
            // is what lets the browser reach the exit at all.
            .arg("--proxy-bypass-list=<-loopback>")
            // WebRTC can open connections that ignore a configured proxy, which is a second door
            // out of the process. Off here as an egress requirement, not a fingerprinting
            // preference (§21.7, failure point 106).
            .arg("--webrtc-ip-handling-policy=disable_non_proxied_udp")
            .arg("--force-webrtc-ip-handling-policy")
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--headless=new")
            .arg("about:blank")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| BrowserError::CouldNotStart(e.to_string()))?;

        Ok(Self {
            child,
            port,
            _exit: exit,
        })
    }

    /// Where the protocol client connects. Loopback, always.
    #[must_use]
    pub fn control_url(&self) -> String {
        format!("http://127.0.0.1:{}/json/version", self.port)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Killed rather than asked. §18.3 drops guards on the cancel path and runs no cleanup code
        // there, and a browser that ignores a polite shutdown would hold the exit open past the
        // turn that opened it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("no Chromium-family browser is installed")]
    NotInstalled,
    #[error("the browser could not be started: {0}")]
    CouldNotStart(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of our own, matching how `ledger.rs` and `journal.rs` already do it
    /// rather than adding a crate for four lines.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(what: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("loki-browser-{what}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch");
            Self(path)
        }

        fn install(&self, app: &str, binary: &str) {
            let dir = self.0.join(app).join("Contents/MacOS");
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join(binary), b"").expect("write");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_accounting_flags_are_all_present() {
        // Two of these are the difference between 31 requests and 2 on one page load, so a silent
        // removal is a silent regression in the byte accounting rather than a tidy-up.
        for required in [
            "--disable-background-networking",
            "--disable-component-update",
            "--no-pings",
        ] {
            assert!(QUIET.contains(&required), "{required} is not optional");
        }
    }

    #[test]
    fn brave_is_preferred_and_every_candidate_names_a_bundle() {
        let candidates = Chromium::candidates();
        assert_eq!(candidates[0].0, "Brave");
        for (name, app, binary) in candidates {
            assert!(app.ends_with(".app"), "{name} names a bundle");
            assert!(!binary.is_empty(), "{name} names an executable");
        }
    }

    /// Detection reads the filesystem, so it is tested against one rather than against the machine.
    #[test]
    fn detection_finds_the_first_installed_and_nothing_when_none_are() {
        let root = Scratch::new("order");
        assert_eq!(Chromium::detect_under(root.path()), None);

        // Second in the order, so this also proves the order is a preference and not an accident.
        root.install("Google Chrome.app", "Google Chrome");
        assert_eq!(
            Chromium::detect_under(root.path()).map(|found| found.name),
            Some("Chrome")
        );

        // Brave arriving later still wins, because the order is the preference.
        root.install("Brave Browser.app", "Brave Browser");
        assert_eq!(
            Chromium::detect_under(root.path()).map(|found| found.name),
            Some("Brave")
        );
    }

    /// A directory named like the executable is not the executable.
    #[test]
    fn a_bundle_without_an_executable_is_not_installed() {
        let root = Scratch::new("hollow");
        std::fs::create_dir_all(
            root.path()
                .join("Brave Browser.app/Contents/MacOS/Brave Browser"),
        )
        .expect("mkdir");
        assert_eq!(Chromium::detect_under(root.path()), None);
    }
}
