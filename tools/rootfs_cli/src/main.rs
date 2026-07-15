use std::process::exit;

use colored::{Color, Colorize};
use log::{Level, LevelFilter, Log, error, info};
use nix::{
    sys::signal::{SigHandler, Signal, kill, signal},
    unistd::Pid,
};

use crate::cli::run_cli;

mod cli;

const LOGGER: ChariotLogger = ChariotLogger;

struct ChariotLogger;

impl Log for ChariotLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= Level::Error
    }

    fn log(&self, record: &log::Record) {
        let level_color = match record.level() {
            Level::Trace => Color::Black,
            Level::Debug => Color::Blue,
            Level::Info => Color::Green,
            Level::Warn => Color::Yellow,
            Level::Error => Color::Red,
        };

        eprintln!("{} | {}", record.level().as_str().color(level_color).bold(), record.args());
    }

    fn flush(&self) {}
}

extern "C" fn handle_sigint(_: nix::libc::c_int) {
    info!("Terminated chariot process ({})", Pid::this());
    kill(Pid::from_raw(0), Signal::SIGKILL).expect("Failed to kill process group");
    exit(1);
}

fn main() {
    unsafe { signal(Signal::SIGINT, SigHandler::Handler(handle_sigint)) }.unwrap();

    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(LevelFilter::Info))
        .expect("Failed to initialize logger");

    if let Err(err) = run_cli() {
        error!("{}", err);
        if err.chain().len() > 1 {
            error!("Caused by:");
            for (i, sub_error) in err.chain().skip(1).enumerate() {
                error!("  {}: {}", i, sub_error)
            }
        }

        exit(1);
    }
}
