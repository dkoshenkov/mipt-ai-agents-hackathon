use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_RUN: AtomicUsize = AtomicUsize::new(0);

/// Shared attempt budget for all stages participating in one review.
pub struct ReviewRun {
    pub(super) id: String,
    limit: usize,
    attempts: AtomicUsize,
}

impl ReviewRun {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub fn new(limit: usize) -> Self {
        Self {
            id: format!(
                "typed-{}-{}",
                std::process::id(),
                NEXT_RUN.fetch_add(1, Ordering::Relaxed)
            ),
            limit,
            attempts: AtomicUsize::new(0),
        }
    }

    /// Number of attempts dispatched, including requests that failed or timed out.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.attempts())
    }

    pub(super) fn reserve(&self) -> Option<usize> {
        let mut used = self.attempts();
        while used < self.limit {
            match self.attempts.compare_exchange_weak(
                used,
                used + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(used + 1),
                Err(actual) => used = actual,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::ReviewRun;
    use std::sync::{Arc, Barrier};

    #[test]
    fn budget_is_atomic_and_independent_for_each_run() {
        let run = Arc::new(ReviewRun::new(1));
        let barrier = Arc::new(Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let run = Arc::clone(&run);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    run.reserve()
                })
            })
            .collect();
        let reservations: Vec<_> = threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect();
        assert_eq!(reservations, [1]);
        assert_eq!(run.attempts(), 1);
        assert_eq!(run.reserve(), None);

        let empty = ReviewRun::new(0);
        assert_eq!(empty.reserve(), None);
        assert_eq!(empty.attempts(), 0);

        let another = ReviewRun::new(1);
        assert_ne!(run.id, another.id);
        assert_eq!(another.reserve(), Some(1));
        assert_eq!(another.attempts(), 1);
    }
}
