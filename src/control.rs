use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a paused worker wakes up to check whether it's been resumed or
/// cancelled.
const PAUSE_POLL: Duration = Duration::from_millis(50);

/// Cancel and pause flags shared between the UI thread and a background
/// worker (scan, index, or delete). Workers call `checkpoint` wherever they
/// previously only polled for cancellation.
#[derive(Debug, Default)]
pub struct JobControl {
    cancelled: AtomicBool,
    paused: AtomicBool,
}

impl JobControl {
    /// A control that's already cancelled, e.g. to test that a worker bails
    /// out immediately.
    pub fn cancelled() -> Self {
        let control = Self::default();
        control.cancel();
        control
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Blocks for as long as the job is paused, then reports whether the
    /// worker should stop. Cancelling a paused job wakes it up (to stop).
    pub fn checkpoint(&self) -> bool {
        while self.is_paused() && !self.is_cancelled() {
            std::thread::sleep(PAUSE_POLL);
        }
        self.is_cancelled()
    }
}

/// Measures how long a job has actually been running, leaving out time spent
/// paused, so rates and ETAs don't sag (and "done in" times don't balloon)
/// just because the user paused for a while.
#[derive(Clone, Copy, Debug)]
pub struct ActiveClock {
    started_at: Instant,
    paused_since: Option<Instant>,
    paused_total: Duration,
}

impl ActiveClock {
    pub fn start() -> Self {
        Self {
            started_at: Instant::now(),
            paused_since: None,
            paused_total: Duration::ZERO,
        }
    }

    /// Like `start`, but begins paused (a clock created while its job is
    /// already paused).
    pub fn start_paused(paused: bool) -> Self {
        let mut clock = Self::start();
        if paused {
            clock.pause();
        }
        clock
    }

    pub fn pause(&mut self) {
        if self.paused_since.is_none() {
            self.paused_since = Some(Instant::now());
        }
    }

    pub fn resume(&mut self) {
        if let Some(since) = self.paused_since.take() {
            self.paused_total += since.elapsed();
        }
    }

    /// Time spent running (not paused) since `start`.
    pub fn elapsed(&self) -> Duration {
        let now = self.paused_since.unwrap_or_else(Instant::now);
        now.duration_since(self.started_at)
            .saturating_sub(self.paused_total)
    }

    /// Total time spent paused so far, including any pause still ongoing.
    pub fn paused_total(&self) -> Duration {
        self.paused_total + self.paused_since.map_or(Duration::ZERO, |s| s.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn checkpoint_blocks_while_paused_until_resumed() {
        let control = Arc::new(JobControl::default());
        control.set_paused(true);
        let worker = {
            let control = control.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                let stop = control.checkpoint();
                (stop, started.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        control.set_paused(false);
        let (stop, waited) = worker.join().unwrap();
        assert!(!stop);
        assert!(waited >= Duration::from_millis(150));
    }

    #[test]
    fn cancelling_a_paused_job_releases_the_worker() {
        let control = Arc::new(JobControl::default());
        control.set_paused(true);
        let worker = {
            let control = control.clone();
            std::thread::spawn(move || control.checkpoint())
        };
        std::thread::sleep(Duration::from_millis(100));
        control.cancel();
        assert!(worker.join().unwrap());
    }

    #[test]
    fn clock_excludes_paused_time() {
        let mut clock = ActiveClock::start();
        std::thread::sleep(Duration::from_millis(50));
        clock.pause();
        let at_pause = clock.elapsed();
        std::thread::sleep(Duration::from_millis(150));
        // Frozen while paused.
        assert_eq!(clock.elapsed(), at_pause);
        clock.resume();
        assert!(clock.elapsed() < Duration::from_millis(150));
        assert!(clock.paused_total() >= Duration::from_millis(150));
    }
}
