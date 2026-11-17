//! Background work that stays out of the way (v0.8).
//!
//! Reading documents, OCR and building vectors can take hours on a big disk.
//! None of it is urgent, so it runs at background priority (macOS throttles
//! its CPU and disk use) and holds still while:
//! - the user pressed Pause,
//! - the Mac runs on battery (when "pause on battery" is on, the default),
//! - Low Power Mode is on.
//!
//! Scanning and the live watcher never wait: finding files is what makes
//! search work at all.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Why background work is holding still right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    User,
    Battery,
    LowPower,
}

/// What the Mac's power looks like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Power {
    pub on_battery: bool,
    pub low_power: bool,
}

static USER_PAUSED: AtomicBool = AtomicBool::new(false);
static PAUSE_ON_BATTERY: AtomicBool = AtomicBool::new(true);

type Probe = fn() -> Power;
struct Cache {
    probe: Probe,
    at: Option<Instant>,
    power: Power,
}
static CACHE: Mutex<Cache> = Mutex::new(Cache { probe: system_power, at: None, power: Power { on_battery: false, low_power: false } });
const POWER_TTL: Duration = Duration::from_secs(20);

pub fn set_paused(on: bool) {
    USER_PAUSED.store(on, Ordering::Relaxed);
}

pub fn paused() -> bool {
    USER_PAUSED.load(Ordering::Relaxed)
}

pub fn set_pause_on_battery(on: bool) {
    PAUSE_ON_BATTERY.store(on, Ordering::Relaxed);
}

/// Replace how power is read (tests, or a shell with a better source).
pub fn set_power_probe(probe: Probe) {
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    c.probe = probe;
    c.at = None;
}

pub fn power() -> Power {
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.at.map(|t| t.elapsed() >= POWER_TTL).unwrap_or(true) {
        c.power = (c.probe)();
        c.at = Some(Instant::now());
    }
    c.power
}

/// The reason to hold, if any. The user's Pause wins.
pub fn hold() -> Option<Hold> {
    if paused() {
        return Some(Hold::User);
    }
    let p = power();
    if p.low_power {
        Some(Hold::LowPower)
    } else if p.on_battery && PAUSE_ON_BATTERY.load(Ordering::Relaxed) {
        Some(Hold::Battery)
    } else {
        None
    }
}

/// Block while background work should hold. Returns false when cancelled.
pub fn wait(cancel: &Arc<AtomicBool>) -> bool {
    wait_with(cancel, Duration::from_secs(2))
}

pub(crate) fn wait_with(cancel: &Arc<AtomicBool>, poll: Duration) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        if hold().is_none() {
            return true;
        }
        std::thread::sleep(poll);
    }
}

/// Run the calling thread at background priority: macOS gives it spare CPU
/// only and throttles its disk reads. Elsewhere, a lower nice value.
pub fn background_thread() {
    #[cfg(target_os = "macos")]
    unsafe {
        const PRIO_DARWIN_THREAD: libc::c_int = 3;
        const PRIO_DARWIN_BG: libc::c_int = 0x1000;
        libc::setpriority(PRIO_DARWIN_THREAD, 0, PRIO_DARWIN_BG);
    }
}

/// A helper process (OCR, embeddings) at background priority too.
pub(crate) fn background_command(cmd: &mut std::process::Command) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                const PRIO_DARWIN_PROCESS: libc::c_int = 4;
                const PRIO_DARWIN_BG: libc::c_int = 0x1000;
                libc::setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG);
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = cmd;
}

/// `pmset -g batt` says where power comes from; `pmset -g` lists
/// `lowpowermode 1` when Low Power Mode is on. A desktop Mac or a failed read
/// counts as plugged in.
fn system_power() -> Power {
    #[cfg(target_os = "macos")]
    {
        let run = |args: &[&str]| {
            std::process::Command::new("/usr/bin/pmset")
                .args(args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        return parse_power(&run(&["-g", "batt"]), &run(&["-g"]));
    }
    #[allow(unreachable_code)]
    Power::default()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_power(batt: &str, settings: &str) -> Power {
    let on_battery = batt.lines().next().map(|l| l.contains("'Battery Power'")).unwrap_or(false);
    let low_power = settings.lines().any(|l| {
        let mut it = l.split_whitespace();
        it.next() == Some("lowpowermode") && it.next() == Some("1")
    });
    Power { on_battery, low_power }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pmset() {
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t82%; discharging;";
        let ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged;";
        let set = "System-wide power settings:\nCurrently in use:\n lowpowermode         1\n sleep                1";
        assert_eq!(parse_power(batt, ""), Power { on_battery: true, low_power: false });
        assert_eq!(parse_power(ac, set), Power { on_battery: false, low_power: true });
        assert_eq!(parse_power("", " lowpowermode 0"), Power::default());
    }

    #[test]
    fn holds_and_resumes() {
        fn battery() -> Power {
            Power { on_battery: true, low_power: false }
        }
        fn plugged() -> Power {
            Power::default()
        }
        set_power_probe(battery);
        assert_eq!(hold(), Some(Hold::Battery));
        set_pause_on_battery(false);
        assert_eq!(hold(), None);
        set_pause_on_battery(true);
        set_power_probe(plugged);
        assert_eq!(hold(), None);
        set_paused(true);
        assert_eq!(hold(), Some(Hold::User));
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let t = std::thread::spawn(move || wait_with(&c2, Duration::from_millis(5)));
        std::thread::sleep(Duration::from_millis(30));
        assert!(!t.is_finished(), "should hold while paused");
        set_paused(false);
        assert!(t.join().unwrap());
        cancel.store(true, Ordering::Relaxed);
        assert!(!wait_with(&cancel, Duration::from_millis(5)));
    }
}
