// use clipboard::StubCliprdrServerFactory;
use counter::IntervalCounter;
use strum::EnumString;

mod config;
mod counter;
// mod clipboard;
// mod credential;
mod gui;
mod input;
mod screen;
mod server;

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
#[strum(ascii_case_insensitive)]
enum Security {
    None,
    Tls,
    Hybrid,
}

fn main() -> Result<(), anyhow::Error> {
    let capture_counter = IntervalCounter::new();
    let display_send_counter = IntervalCounter::new();

    let capture_counter_interval = capture_counter.interval();
    let display_send_counter_interval = display_send_counter.interval();

    use std::fs::OpenOptions;
    use tracing_oslog::OsLogger;
    use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*, EnvFilter};

    // Create log file
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/arisu.log")
        .expect("Failed to create log file");

    let (non_blocking_file, _guard) = tracing_appender::non_blocking(log_file);

    // Create a layered subscriber that sends error/warn to Console, all logs to stdout, and all logs to file
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env().add_directive(LevelFilter::INFO.into()))
        .with(
            OsLogger::new("app.perlmint.arisu", "default").with_filter(LevelFilter::WARN), // Only send warn/error to Console
        )
        .with(
            fmt::layer().with_filter(LevelFilter::INFO), // Send info and above to stdout
        )
        .with(
            fmt::layer()
                .with_writer(non_blocking_file)
                .with_filter(LevelFilter::DEBUG), // Send debug and above to file
        )
        .init();

    // Keep the guard alive to prevent dropping the log writer
    std::mem::forget(_guard);

    // Create server controller
    let server_controller = server::ServerController::new(capture_counter, display_send_counter)?;

    gui::run(
        capture_counter_interval,
        display_send_counter_interval,
        server_controller,
    );

    Ok(())
}
