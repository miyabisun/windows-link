use std::{fs::OpenOptions, path::Path, sync::Mutex};

use tracing_subscriber::filter::LevelFilter;

fn level() -> LevelFilter {
    match std::env::var("LOG_LEVEL").as_deref() {
        Ok("off") => LevelFilter::OFF,
        Ok("error") => LevelFilter::ERROR,
        Ok("warn") => LevelFilter::WARN,
        Ok("debug") => LevelFilter::DEBUG,
        Ok("trace") => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    }
}

/// Logs to stderr when a console is attached, otherwise appends to `file`
/// (the release build runs without a console when started at logon).
pub fn init(file: Option<&Path>) {
    let builder = tracing_subscriber::fmt().with_max_level(level());
    let opened = file.and_then(|path| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        OpenOptions::new().create(true).append(true).open(path).ok()
    });
    match opened {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .init(),
        None => builder.with_writer(std::io::stderr).init(),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    const MARKERS: [&str; 5] = [
        "log-probe-error",
        "log-probe-warn",
        "log-probe-info",
        "log-probe-debug",
        "log-probe-trace",
    ];

    #[test]
    #[ignore = "executed in a child process to isolate the global subscriber and environment"]
    fn emit_logs() {
        super::init(None);
        tracing::error!("log-probe-error");
        tracing::warn!("log-probe-warn");
        tracing::info!("log-probe-info");
        tracing::debug!("log-probe-debug");
        tracing::trace!("log-probe-trace");
    }

    #[test]
    fn initialized_logger_obeys_log_level() {
        let cases = [
            (None, 3),
            (Some("off"), 0),
            (Some("error"), 1),
            (Some("warn"), 2),
            (Some("info"), 3),
            (Some("debug"), 4),
            (Some("trace"), 5),
            (Some(""), 3),
            (Some("invalid"), 3),
            (Some("DEBUG"), 3),
            (Some("debug,tower_http=trace"), 3),
        ];
        for (level, emitted) in cases {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "logging::tests::emit_logs",
                    "--ignored",
                    "--nocapture",
                ])
                .env_remove("LOG_LEVEL")
                .env("RUST_LOG", "trace");
            if let Some(level) = level {
                command.env("LOG_LEVEL", level);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let logs = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            for (index, marker) in MARKERS.iter().enumerate() {
                assert_eq!(
                    logs.contains(marker),
                    index < emitted,
                    "LOG_LEVEL={level:?}, marker={marker}: {logs}",
                );
            }
        }
    }
}
