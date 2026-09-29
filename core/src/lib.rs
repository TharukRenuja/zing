pub mod bwschedule;
pub mod connection;
pub mod constants;
pub mod cookie_store;
pub mod downloader;
pub mod engine;
pub mod http_method;
pub mod probe;
pub mod ratelimit;
pub mod retry;
pub mod rpc;
pub mod segment;
pub mod storage;
pub mod transport;
pub mod util;

// Formerly the separate `zing-ext` crate. The engine already depended on all of
// these — chunk-hash verification, digest auth, the Content-Disposition
// filename, rate-string parsing — so they were engine code living behind a
// separate manifest that everything else also had to depend on just to print a
// byte count.
pub mod bandwidth;
pub mod checksum;
pub mod digest_auth;
pub mod filename;
pub mod metalink;

/// Display formatting. The only part of this set that is presentation rather
/// than engine behaviour, kept here so a frontend needs one dependency instead
/// of two.
pub mod human;

pub use engine::event::EventBus;
pub use http_method::{HttpMethod, RequestBody, RequestSpec};
pub use metalink::{ChunkHashes, HashAlgorithm, MetalinkFile};
