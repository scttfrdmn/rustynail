use sha2::{Digest, Sha256};
use std::collections::VecDeque;

/// Deduplicates messages using a SHA-256 ring buffer.
///
/// The ring buffer stores hashes of `"user_id:content"` strings.  When the
/// buffer reaches capacity the oldest entry is evicted (FIFO).  This means
/// deduplication only covers the most recent `capacity` messages — old enough
/// duplicates are treated as new.
pub struct MessageDeduplicator {
    window: VecDeque<[u8; 32]>,
    capacity: usize,
}

impl MessageDeduplicator {
    pub fn new(capacity: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Returns `true` if this `(user_id, content)` pair was seen recently.
    ///
    /// If it was NOT seen before, the hash is added to the ring buffer and
    /// `false` is returned.
    pub fn seen(&mut self, user_id: &str, content: &str) -> bool {
        let key: [u8; 32] = Sha256::digest(format!("{}:{}", user_id, content)).into();
        if self.window.contains(&key) {
            return true;
        }
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(key);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_message_not_duplicate() {
        let mut d = MessageDeduplicator::new(8);
        assert!(!d.seen("user1", "hello"));
    }

    #[test]
    fn identical_message_is_duplicate() {
        let mut d = MessageDeduplicator::new(8);
        d.seen("user1", "hello");
        assert!(d.seen("user1", "hello"));
    }

    #[test]
    fn different_user_same_content_not_duplicate() {
        let mut d = MessageDeduplicator::new(8);
        d.seen("user1", "hello");
        assert!(!d.seen("user2", "hello"));
    }

    #[test]
    fn ring_buffer_eviction() {
        let mut d = MessageDeduplicator::new(2);
        d.seen("u", "a");
        d.seen("u", "b");
        // "a" should have been evicted
        d.seen("u", "c");
        assert!(!d.seen("u", "a"), "evicted entry should not be detected as duplicate");
    }
}
