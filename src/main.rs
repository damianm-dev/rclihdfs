mod cli;
mod commands;
mod config;
mod db;
mod error;
mod hdfs;
mod kerberos;
mod policy;
mod proc;

use clap::{CommandFactory, Parser};
use error::CliError;

fn run() -> Result<(), CliError> {
    if std::env::args().len() == 1 {
        cli::Cli::command().print_help().ok();
        eprintln!();
        std::process::exit(1);
    }

    let args = cli::Cli::parse();

    config::load_conf()?;
    let user = config::current_username()?;
    let cfg = config::build_auth_config()?;
    let layout = config::build_layout()?;

    match args.command {
        cli::Command::Cp { source, target } => {
            commands::do_cp(&source, &target, &user, &cfg, &layout)
        }
        cli::Command::Mv {
            source,
            target,
            yes,
        } => commands::do_mv(&source, &target, yes, &user, &cfg, &layout),
        cli::Command::Rm { source, yes } => commands::do_rm(&source, yes, &user, &cfg, &layout),
        cli::Command::Mkdir { path } => commands::do_mkdir(&path, &user, &cfg, &layout),
    }
}

/// Formats a log line in Python `logging` style (`%(asctime)s %(levelname)s
/// %(name)s: %(message)s`): local time with milliseconds, `WARNING` for warn.
fn format_log_line(now: chrono::DateTime<chrono::Local>, level: log::Level, msg: &str) -> String {
    let level = match level {
        log::Level::Warn => "WARNING",
        other => other.as_str(),
    };
    format!(
        "{} {level} rclihdfs: {msg}",
        now.format("%Y-%m-%d %H:%M:%S,%3f")
    )
}

fn main() {
    proc::install_interrupt_handler();

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .format(|buf, record| {
            use std::io::Write;
            let line = format_log_line(
                chrono::Local::now(),
                record.level(),
                &record.args().to_string(),
            );
            writeln!(buf, "{line}")
        })
        .init();

    if let Err(e) = run() {
        match &e {
            CliError::Policy(_) | CliError::NotFound(_) => log::warn!("{e}"),
            CliError::Runtime(_) => log::error!("{e}"),
        }
        std::process::exit(e.exit_code());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn log_line_matches_python_format() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 3, 16, 22, 14)
            .unwrap()
            + chrono::Duration::milliseconds(50);

        assert_eq!(
            format_log_line(now, log::Level::Info, "SUCCESS"),
            "2026-10-03 16:22:14,050 INFO rclihdfs: SUCCESS"
        );
        assert_eq!(
            format_log_line(now, log::Level::Warn, "x"),
            "2026-10-03 16:22:14,050 WARNING rclihdfs: x"
        );
    }
}
