//! Terminal-only bounded logging. Producers never wait for terminal I/O.
use std::{
    io::{self, IsTerminal, Write},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

struct Logger {
    sender: mpsc::SyncSender<String>,
    start: Instant,
    dropped: AtomicU64,
}
static LOGGER: OnceLock<Option<Logger>> = OnceLock::new();

fn logger() -> Option<&'static Logger> {
    LOGGER
        .get_or_init(|| {
            if !io::stderr().is_terminal() && !io::stdout().is_terminal() {
                return None;
            }
            let (sender, receiver) = mpsc::sync_channel::<String>(256);
            thread::Builder::new()
                .name("terminal-log".into())
                .spawn(move || {
                    let mut output: Box<dyn Write> = if io::stderr().is_terminal() {
                        Box::new(io::stderr())
                    } else {
                        Box::new(io::stdout())
                    };
                    for line in receiver {
                        if writeln!(output, "{line}").is_err() {
                            break;
                        }
                    }
                })
                .ok()?;
            Some(Logger {
                sender,
                start: Instant::now(),
                dropped: AtomicU64::new(0),
            })
        })
        .as_ref()
}

pub fn enabled() -> bool {
    logger().is_some()
}

#[allow(non_snake_case)]
pub fn Print(level: Level, message: impl std::fmt::Display) {
    let Some(logger) = logger() else {
        return;
    };
    let dropped = logger.dropped.swap(0, Ordering::Relaxed);
    let line = format!(
        "[{:.3} {level:?}] {message}{}",
        logger.start.elapsed().as_secs_f64(),
        if dropped == 0 {
            String::new()
        } else {
            format!(" [dropped {dropped} log messages]")
        }
    );
    if logger.sender.try_send(line).is_err() {
        logger.dropped.fetch_add(dropped + 1, Ordering::Relaxed);
    }
}
