//! Log output matching upstream's logger.py byte-for-byte where tests grep it.
//!
//! JSON:   {"time": "<iso8601 µs + offset>", "level": "INFO", "msg": "..."}
//! LOGFMT: time=<iso8601> level=INFO msg="..."
//!
//! LOG_CONFIG is a Python `logging.dictConfig` YAML upstream. Full dictConfig
//! semantics cannot be replicated outside Python; we interpret the subset the
//! ecosystem actually uses (and the upstream test suite exercises): root level,
//! one console handler that is either a StreamHandler or a
//! RotatingFileHandler(filename, maxBytes, backupCount), and a JSON or LOGFMT
//! formatter. This is a documented deviation (see NOTES.md).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 10,
    Info = 20,
    Warning = 30,
    Error = 40,
    Critical = 50,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
            Level::Critical => "CRITICAL",
        }
    }
    fn parse(s: &str) -> Option<Level> {
        match s.to_uppercase().as_str() {
            "DEBUG" => Some(Level::Debug),
            "INFO" => Some(Level::Info),
            "WARNING" | "WARN" => Some(Level::Warning),
            "ERROR" => Some(Level::Error),
            "CRITICAL" | "FATAL" => Some(Level::Critical),
            _ => None,
        }
    }
}

enum Format {
    Json,
    Logfmt,
}

enum Sink {
    Stderr,
    RotatingFile {
        path: PathBuf,
        max_bytes: u64,
        backup_count: u32,
        file: Mutex<File>,
    },
}

struct Logger {
    level: Level,
    format: Format,
    utc: bool,
    sink: Sink,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Initialise from LOG_LEVEL / LOG_FORMAT / LOG_TZ / LOG_CONFIG.
/// Exit codes on bad LOG_CONFIG mirror upstream (1 missing file, 2 bad yaml).
pub fn init() {
    let mut level = std::env::var("LOG_LEVEL")
        .ok()
        .and_then(|s| Level::parse(&s))
        .unwrap_or(Level::Info);
    let mut format = match std::env::var("LOG_FORMAT")
        .unwrap_or_default()
        .to_uppercase()
        .as_str()
    {
        "LOGFMT" => Format::Logfmt,
        _ => Format::Json,
    };
    let utc = std::env::var("LOG_TZ").unwrap_or_default().to_uppercase() == "UTC";
    let mut sink = Sink::Stderr;

    if let Ok(conf_path) = std::env::var("LOG_CONFIG")
        && !conf_path.is_empty()
    {
        let text = match std::fs::read_to_string(&conf_path) {
            Ok(t) => t,
            Err(_) => {
                // Upstream: plain print + sys.exit(1)
                println!("Config file: {} Not Found", conf_path);
                std::process::exit(1);
            }
        };
        let doc: serde_yaml::Value = match serde_yaml::from_str(&text) {
            Ok(d) => d,
            Err(e) => {
                println!("Error loading yaml file:");
                println!("{}", e);
                std::process::exit(2);
            }
        };
        if let Some(l) = doc["root"]["level"]
            .as_str()
            .or_else(|| doc["handlers"]["console"]["level"].as_str())
            .and_then(Level::parse)
        {
            level = l;
        }
        let handler = &doc["handlers"]["console"];
        if let Some(fmt_name) = handler["formatter"].as_str() {
            if let Some(cls) = doc["formatters"][fmt_name]["()"].as_str() {
                format = if cls.contains("Logfmt") {
                    Format::Logfmt
                } else {
                    Format::Json
                };
            } else if fmt_name.to_uppercase().contains("LOGFMT") {
                format = Format::Logfmt;
            }
        }
        if handler["class"]
            .as_str()
            .is_some_and(|c| c.contains("FileHandler"))
            && let Some(filename) = handler["filename"].as_str()
        {
            let path = PathBuf::from(filename);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => {
                    sink = Sink::RotatingFile {
                        path,
                        max_bytes: handler["maxBytes"].as_u64().unwrap_or(0),
                        backup_count: handler["backupCount"].as_u64().unwrap_or(0) as u32,
                        file: Mutex::new(f),
                    }
                }
                Err(e) => {
                    println!("Error loading yaml file:");
                    println!("cannot open log file {}: {}", filename, e);
                    std::process::exit(2);
                }
            }
        }
    }

    let _ = LOGGER.set(Logger {
        level,
        format,
        utc,
        sink,
    });
}

fn timestamp(utc: bool) -> String {
    if utc {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
            .to_string()
    } else {
        chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
            .to_string()
    }
}

fn logfmt_value(v: &str) -> String {
    if v.is_empty() || v.contains(' ') || v.contains('=') || v.contains('"') {
        format!(
            "\"{}\"",
            v.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
    } else {
        v.to_string()
    }
}

fn emit(level: Level, msg: &str) {
    let Some(l) = LOGGER.get() else {
        eprintln!("{} {}", level.name(), msg);
        return;
    };
    if level < l.level {
        return;
    }
    let ts = timestamp(l.utc);
    let line = match l.format {
        Format::Json => format!(
            "{{\"time\": {}, \"level\": {}, \"msg\": {}}}\n",
            serde_json::to_string(&ts).unwrap(),
            serde_json::to_string(level.name()).unwrap(),
            serde_json::to_string(msg).unwrap()
        ),
        Format::Logfmt => format!(
            "time={} level={} msg={}\n",
            ts,
            level.name(),
            logfmt_value(msg)
        ),
    };
    match &l.sink {
        Sink::Stderr => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        Sink::RotatingFile {
            path,
            max_bytes,
            backup_count,
            file,
        } => {
            let mut f = file.lock().unwrap();
            if *max_bytes > 0 {
                let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                if size + line.len() as u64 > *max_bytes && *backup_count > 0 {
                    // RotatingFileHandler-style rollover: .1 is the newest backup.
                    let _ = std::fs::remove_file(bak(path, *backup_count));
                    for i in (1..*backup_count).rev() {
                        let _ = std::fs::rename(bak(path, i), bak(path, i + 1));
                    }
                    drop(std::mem::replace(
                        &mut *f,
                        // Temporarily point at /dev/null while we rotate.
                        OpenOptions::new().append(true).open("/dev/null").unwrap(),
                    ));
                    let _ = std::fs::rename(path, bak(path, 1));
                    if let Ok(nf) = OpenOptions::new().create(true).append(true).open(path) {
                        *f = nf;
                    }
                }
            }
            let _ = f.write_all(line.as_bytes());
        }
    }
}

fn bak(path: &std::path::Path, i: u32) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), i))
}

pub fn debug(msg: &str) {
    emit(Level::Debug, msg);
}
pub fn info(msg: &str) {
    emit(Level::Info, msg);
}
pub fn warning(msg: &str) {
    emit(Level::Warning, msg);
}
pub fn error(msg: &str) {
    emit(Level::Error, msg);
}
/// Python's logger.fatal is an alias for CRITICAL; it does not exit.
pub fn fatal(msg: &str) {
    emit(Level::Critical, msg);
}
