//! What a session leaves behind: its log, on stderr and in a file (a
//! Finder launch has no terminal to read), and a record of any panic — the
//! message, a backtrace, and a mark the next launch finds and says so.

use std::backtrace::Backtrace;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Past this the log starts over, the previous one kept beside it: a
/// panic's record survives the next session's chatter.
const MAX_LOG: u64 = 1024 * 1024;

/// Left in the data directory by a panic; the next launch takes it.
pub const CRASH_MARK: &str = "crashed";

/// `~/Library/Logs/com.lauf.jiji/Jiji.log`, where every release has kept
/// it. A run with a data directory of its own (a test, a scripted run)
/// keeps its log there instead, never in the real one.
pub fn log_path() -> PathBuf {
    let data_dir = crate::store::data_dir();
    if std::env::var_os("JIJI_DATA_DIR").is_some() || !cfg!(target_os = "macos") {
        return data_dir.join("Jiji.log");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Logs")
        .join(crate::store::APP_ID)
        .join("Jiji.log")
}

/// The log file, appended to line by line: nothing waits in a buffer for
/// a crash to lose.
struct LogFile {
    path: PathBuf,
    file: File,
    size: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(Self { path, file, size })
    }

    fn write(&mut self, line: &str) {
        if self.size + line.len() as u64 > MAX_LOG {
            let _ = std::fs::rename(&self.path, self.path.with_extension("old.log"));
            match Self::open(self.path.clone()) {
                Ok(fresh) => *self = fresh,
                Err(err) => eprintln!("jiji: could not start a new log: {err}"),
            }
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.size += line.len() as u64;
        }
    }
}

/// Local time with its offset, to the millisecond.
fn stamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f %z")
        .to_string()
}

struct Logger {
    file: Mutex<Option<LogFile>>,
}

static LOGGER: Logger = Logger {
    file: Mutex::new(None),
};

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        // GPUI and its dependencies are chatty at info; keep theirs to
        // warnings.
        let ours = record.target().starts_with("jiji");
        if !self.enabled(record.metadata()) || !(ours || record.level() <= log::Level::Warn) {
            return;
        }
        let line = format!(
            "[{}] {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        eprintln!("{line}");
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                file.write(&format!("{} {line}\n", stamp()));
            }
        }
    }

    fn flush(&self) {}
}

/// Log to stderr and the log file, and record any panic there.
pub fn install() {
    let path = log_path();
    match LogFile::open(path.clone()) {
        Ok(file) => {
            if let Ok(mut slot) = LOGGER.file.lock() {
                *slot = Some(file);
            }
        }
        Err(err) => eprintln!("jiji: no log file at {}: {err}", path.display()),
    }
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    let mark = crate::store::data_dir().join(CRASH_MARK);
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(&path, &mark, info);
        report(info);
    }));
}

/// A panic's message, where it struck and the stack that led there,
/// appended by a handle of its own — the logger's may be the one that was
/// busy when it struck — and the mark for the next launch.
fn record_panic(log: &Path, mark: &Path, info: &PanicHookInfo) {
    let thread = std::thread::current();
    let record = format!(
        "{} [PANIC] thread '{}' {info}\n{}\n",
        stamp(),
        thread.name().unwrap_or("<unnamed>"),
        Backtrace::force_capture()
    );
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
        let _ = file.write_all(record.as_bytes());
    }
    let _ = std::fs::write(mark, stamp());
}

/// Whether the last session ended in a panic; the mark is gone after.
pub fn take_crash_mark(data_dir: &Path) -> bool {
    std::fs::remove_file(data_dir.join(CRASH_MARK)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_stays_bounded_and_keeps_its_previous_part() {
        let dir = crate::testing::scratch("log-rotation");
        let (path, previous) = (dir.join("Jiji.log"), dir.join("Jiji.old.log"));
        let read = |path: &Path| std::fs::read_to_string(path).unwrap();
        let mut log = LogFile::open(path.clone()).unwrap();
        log.write("the first line\n");
        let line = format!("{}\n", "x".repeat(999));
        let mut written = "the first line\n".len() as u64;
        while written + 1000 <= MAX_LOG {
            log.write(&line);
            written += 1000;
        }
        assert!(!previous.exists());
        // The next line no longer fits: the log starts over, and the part
        // it had is kept beside it.
        log.write(&line);
        assert_eq!(read(&path), line);
        assert!(read(&previous).starts_with("the first line\n"));
        assert_eq!(std::fs::metadata(&previous).unwrap().len(), written);

        // A session that opens a full log starts it over at its first line.
        drop(log);
        std::fs::write(&path, vec![b'x'; MAX_LOG as usize]).unwrap();
        let mut reopened = LogFile::open(path.clone()).unwrap();
        reopened.write("a new session\n");
        assert_eq!(read(&path), "a new session\n");
        assert_eq!(std::fs::metadata(&previous).unwrap().len(), MAX_LOG);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_panic_is_recorded_with_its_backtrace_for_the_next_launch() {
        let dir = crate::testing::scratch("panic-record");
        let (log, mark) = (dir.join("Jiji.log"), dir.join(CRASH_MARK));
        let report = std::panic::take_hook();
        std::panic::set_hook(Box::new({
            let (log, mark) = (log.clone(), mark.clone());
            move |info| record_panic(&log, &mark, info)
        }));
        let caught = std::panic::catch_unwind(|| panic!("a section fell over"));
        std::panic::set_hook(report);
        assert!(caught.is_err());

        let record = std::fs::read_to_string(&log).unwrap();
        assert!(record.contains("[PANIC] thread '"), "{record}");
        assert!(record.contains("panicked at") && record.contains("diagnostics.rs"));
        assert!(record.contains("a section fell over"));
        // The stack that led there, down to this test.
        assert!(
            record.contains("a_panic_is_recorded_with_its_backtrace"),
            "{record}"
        );
        assert!(take_crash_mark(&dir));
        assert!(!take_crash_mark(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }
}
