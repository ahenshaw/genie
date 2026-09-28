//! Slowing down password guessing: after [`MAX_FAILURES`] wrong passwords
//! within [`WINDOW`] for the same username, or from the same address,
//! sign-ins for it are refused for [`LOCKOUT`].
//!
//! Kept in memory: a restart forgets it, which is fine for a guard whose job
//! is to make guessing slow rather than impossible.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_FAILURES: u32 = 5;
pub const WINDOW: Duration = Duration::from_secs(15 * 60);
pub const LOCKOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct Throttle {
    entries: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    failures: u32,
    since: Instant,
    locked_until: Option<Instant>,
}

impl Throttle {
    /// How long until these may try again, if any of them is locked out.
    pub fn locked(&self, keys: &[String]) -> Option<Duration> {
        let now = Instant::now();
        let entries = self.entries.lock().unwrap();
        keys.iter().filter_map(|k| entries.get(k)?.locked_until).filter(|&t| t > now).map(|t| t - now).max()
    }

    pub fn failed(&self, keys: &[String]) {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        // Forget stale entries now and then, so the map doesn't grow forever.
        if entries.len() > 10_000 {
            entries.retain(|_, e| now.duration_since(e.since) < WINDOW || e.locked_until.is_some_and(|t| t > now));
        }
        for k in keys {
            let e = entries.entry(k.clone()).or_insert(Entry { failures: 0, since: now, locked_until: None });
            if now.duration_since(e.since) > WINDOW {
                *e = Entry { failures: 0, since: now, locked_until: None };
            }
            e.failures += 1;
            if e.failures >= MAX_FAILURES {
                e.locked_until = Some(now + LOCKOUT);
            }
        }
    }

    pub fn succeeded(&self, keys: &[String]) {
        let mut entries = self.entries.lock().unwrap();
        for k in keys {
            entries.remove(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_repeated_failures_until_success_clears_it() {
        let t = Throttle::default();
        let keys = vec!["u:ann".to_string()];
        for _ in 0..MAX_FAILURES - 1 {
            t.failed(&keys);
        }
        assert!(t.locked(&keys).is_none());
        t.failed(&keys);
        assert!(t.locked(&keys).is_some());
        assert!(t.locked(&["u:bob".to_string()]).is_none());
        t.succeeded(&keys);
        assert!(t.locked(&keys).is_none());
    }
}
