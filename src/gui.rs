use crate::counter::Interval;

pub struct App {
    capture_interval: Interval,
    display_send_interval: Interval,
}

impl App {
    pub fn new(capture_interval: Interval, display_send_interval: Interval) -> Self {
        tui_logger::init_logger(tui_logger::LevelFilter::Trace).unwrap();
        tui_logger::set_default_level(tui_logger::LevelFilter::Info);

        Self {
            capture_interval,
            display_send_interval,
        }
    }
}
