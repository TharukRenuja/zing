pub mod engine;
pub mod segment;
pub mod storage;
pub mod downloader;
pub mod connection;
pub mod ratelimit;
pub mod probe;
pub mod retry;
pub mod bwschedule;
pub mod util;

pub use engine::event::EventBus;

pub struct Rxdl {
    pub event_bus: EventBus,
}

impl Rxdl {
    pub fn new() -> Self {
        Self {
            event_bus: EventBus::new(),
        }
    }
}

impl Default for Rxdl {
    fn default() -> Self {
        Self::new()
    }
}
