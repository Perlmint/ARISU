use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

#[derive(Clone)]
pub struct IntervalCounter {
    last_time: Instant,
    interval: Arc<AtomicU64>, // unit: micro seconds
}

impl IntervalCounter {
    pub fn new() -> Self {
        Self {
            last_time: Instant::now(),
            interval: Arc::new(AtomicU64::new(1000000)),
        }
    }

    pub fn update(&mut self) {
        let now = Instant::now();
        let duration = now.duration_since(self.last_time);
        self.last_time = now;
        self.interval
            .store(duration.as_micros() as u64, Ordering::Release);
    }

    pub fn interval(&self) -> Interval {
        Interval(Arc::clone(&self.interval))
    }
}

pub struct Interval(Arc<AtomicU64>);

impl Interval {
    pub fn get(&self) -> std::time::Duration {
        std::time::Duration::from_micros(self.0.load(Ordering::Relaxed))
    }
}
