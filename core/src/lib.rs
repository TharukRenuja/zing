pub mod bwschedule;
pub mod connection;
pub mod constants;
pub mod cookie_store;
pub mod downloader;
pub mod engine;
pub mod probe;
pub mod ratelimit;
pub mod retry;
pub mod segment;
pub mod storage;
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
