//! A fixed-window rate limiter keyed by string. Enough for one process; a fleet would share a store.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// At most `limit` hits per key per `window`.
#[derive(Debug)]
pub struct RateLimit {
    limit: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimit {
    /// A limiter.
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one hit; `false` when the key is now over the limit.
    pub fn allow(&self, key: &str) -> bool {
        self.count(key, true) <= self.limit
    }

    /// Whether the key is already over the limit, without counting.
    pub fn exhausted(&self, key: &str) -> bool {
        self.count(key, false) >= self.limit
    }

    /// Takes back one hit, for a slot reserved with `allow` that should not count.
    pub fn release(&self, key: &str) {
        if let Some(entry) = self.hits.lock().expect("limiter").get_mut(key) {
            entry.1 = entry.1.saturating_sub(1);
        }
    }

    fn count(&self, key: &str, add: bool) -> u32 {
        let now = Instant::now();
        let mut hits = self.hits.lock().expect("limiter");
        if hits.len() > 10_000 {
            hits.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        }
        let entry = hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if add {
            entry.1 += 1;
        }
        entry.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_key_within_the_window() {
        let limit = RateLimit::new(2, Duration::from_secs(60));
        assert!(limit.allow("a"));
        assert!(limit.allow("a"));
        assert!(!limit.allow("a"));
        assert!(limit.allow("b"));
        assert!(!limit.exhausted("b"));
        assert!(limit.allow("b"));
        assert!(limit.exhausted("b"));
        limit.release("b");
        assert!(!limit.exhausted("b"));
        limit.release("b");
        limit.release("b");
        limit.release("never seen");
        assert!(
            limit.allow("b") && limit.allow("b"),
            "release saturates at zero"
        );
        assert!(!limit.allow("b"));
        let short = RateLimit::new(1, Duration::from_millis(1));
        assert!(short.allow("a"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(short.allow("a"), "a new window");
    }
}
