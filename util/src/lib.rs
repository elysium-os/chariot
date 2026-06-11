use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod fs;
pub mod lock;

pub fn current_time() -> Duration {
    // Unwrap deemed safe, UNIX_EPOCH is zero, call should never fail
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
}

pub fn current_timestamp() -> u64 {
    current_time().as_secs()
}
