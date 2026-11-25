use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const IDX_MASK: u8 = 0b11;
const NEW_BIT: u8 = 0b100;

struct TripleBufferInner<T> {
    slots: [UnsafeCell<Option<T>>; 3],
    shared: AtomicU8,
}

// SAFETY: Access to each of the 3 slots is partitioned exclusively among
// (producer write_idx), (shared ready_idx), and (consumer read_idx) via
// atomic swap with AcqRel ordering on `shared`.
unsafe impl<T: Send> Send for TripleBufferInner<T> {}
unsafe impl<T: Send> Sync for TripleBufferInner<T> {}

/// Single-producer half of a wait-free / lock-free triple buffer.
pub struct TripleProducer<T> {
    inner: Arc<TripleBufferInner<T>>,
    write_idx: u8,
}

/// Single-consumer half of a wait-free / lock-free triple buffer.
pub struct TripleConsumer<T> {
    inner: Arc<TripleBufferInner<T>>,
    read_idx: u8,
}

/// Creates a lock-free single-producer, single-consumer triple buffer.
pub fn triple_buffer<T: Send>() -> (TripleProducer<T>, TripleConsumer<T>) {
    let inner = Arc::new(TripleBufferInner {
        slots: [
            UnsafeCell::new(None),
            UnsafeCell::new(None),
            UnsafeCell::new(None),
        ],
        // Initial partition: write_idx = 0, read_idx = 1, shared ready_idx = 2 (no new item).
        shared: AtomicU8::new(2),
    });
    (
        TripleProducer {
            inner: Arc::clone(&inner),
            write_idx: 0,
        },
        TripleConsumer { inner, read_idx: 1 },
    )
}

impl<T> TripleProducer<T> {
    /// Publishes a new value into the triple buffer without blocking.
    ///
    /// Overwrites any unread value in the shared middle slot if the consumer
    /// has not yet polled it.
    pub fn publish(&mut self, value: T) {
        let w = self.write_idx;
        // SAFETY: `self.write_idx` is exclusively owned by this producer until
        // swapped into `self.inner.shared` below.
        unsafe {
            *self.inner.slots[w as usize].get() = Some(value);
        }
        let prev = self.inner.shared.swap(w | NEW_BIT, Ordering::AcqRel);
        self.write_idx = prev & IDX_MASK;
    }
}

impl<T> TripleConsumer<T> {
    /// Returns `true` if the producer has published a value not yet observed by `poll`.
    pub fn has_pending(&self) -> bool {
        (self.inner.shared.load(Ordering::Acquire) & NEW_BIT) != 0
    }

    /// Polls for a newly published value.
    ///
    /// Returns `Some(&T)` only if a fresh item arrived since the previous `poll`,
    /// keeping the item stored in the consumer's current slot for [`Self::current`].
    pub fn poll(&mut self) -> Option<&T> {
        let state = self.inner.shared.load(Ordering::Relaxed);
        if (state & NEW_BIT) == 0 {
            return None;
        }
        let prev = self.inner.shared.swap(self.read_idx, Ordering::AcqRel);
        self.read_idx = prev & IDX_MASK;
        // SAFETY: `self.read_idx` is now exclusively owned by this consumer and
        // `Acquire` synchronizes with the producer's `Release` on `swap`.
        unsafe { (*self.inner.slots[self.read_idx as usize].get()).as_ref() }
    }

    /// Returns a reference to the consumer's currently held value (without consuming new updates).
    pub fn current(&self) -> Option<&T> {
        // SAFETY: `self.read_idx` is exclusively owned by this consumer.
        unsafe { (*self.inner.slots[self.read_idx as usize].get()).as_ref() }
    }

    /// Clears the consumer's currently held value and drops any pending unread value.
    pub fn clear(&mut self) {
        let _ = self.poll();
        // SAFETY: `self.read_idx` is exclusively owned by this consumer.
        unsafe {
            *self.inner.slots[self.read_idx as usize].get() = None;
        }
    }
}

impl<T: Clone> TripleConsumer<T> {
    /// Clones and returns the latest item only when a new item was published since the last poll.
    pub fn poll_cloned(&mut self) -> Option<T> {
        self.poll().cloned()
    }

    /// Clones and returns the currently retained item.
    pub fn current_cloned(&self) -> Option<T> {
        self.current().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_buffer_overwrites_stale_and_retains_current() {
        let (mut tx, mut rx) = triple_buffer::<u64>();
        assert_eq!(rx.poll_cloned(), None);
        assert_eq!(rx.current_cloned(), None);

        tx.publish(1);
        tx.publish(2);
        tx.publish(3);

        // Consumer only wakes up once and receives the newest value (3).
        assert!(rx.has_pending());
        assert_eq!(rx.poll_cloned(), Some(3));
        assert!(!rx.has_pending());
        assert_eq!(rx.poll_cloned(), None);
        assert_eq!(rx.current_cloned(), Some(3));

        tx.publish(4);
        assert_eq!(rx.poll_cloned(), Some(4));
        assert_eq!(rx.current_cloned(), Some(4));

        rx.clear();
        assert_eq!(rx.current_cloned(), None);
        assert_eq!(rx.poll_cloned(), None);
    }

    #[test]
    fn triple_buffer_concurrent_spsc_monotonic() {
        let (mut tx, mut rx) = triple_buffer::<u64>();
        let handle = std::thread::spawn(move || {
            for i in 1..=50_000 {
                tx.publish(i);
            }
        });

        let mut last = 0u64;
        while last < 50_000 {
            if let Some(v) = rx.poll_cloned() {
                assert!(v > last, "non-monotonic value: {v} <= {last}");
                last = v;
            } else {
                std::thread::yield_now();
            }
        }
        handle.join().unwrap();
        assert_eq!(rx.current_cloned(), Some(50_000));
    }
}
