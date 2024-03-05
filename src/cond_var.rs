use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{futex_wake, mutex, resumed_futex_wait, FutexWaitContext, WakeWaiters, U31};

pub struct CondVar {
    counter: AtomicU32,
    waiters: AtomicUsize,
}
impl CondVar {
    pub fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Could be a spurious wake-up
    pub fn wait<'a, T>(&self, m: mutex::MutexGuard<'a, T>) -> mutex::MutexGuard<'a, T> {
        self.waiters.fetch_add(1, Ordering::Relaxed);
        let c = self.counter.load(Ordering::Relaxed);
        let m = m.unlock();

        if let Err(e) = resumed_futex_wait(FutexWaitContext {
            word: &self.counter,
            expected: c,
            timeout: None,
        }) {
            if !matches!(e.kind(), std::io::ErrorKind::WouldBlock) {
                panic!("{e}");
            }
        }

        m.lock()
    }

    pub fn notify_one(&self) {
        if self.waiters.load(Ordering::Relaxed) == 0 {
            return;
        }
        // Because the implementation of `FUTEX_WAKE` has already called `smp_mb()`, `self.counter` has always been incremented before the other thread is waken.
        // - References:
        //   - futex implementation: <https://elixir.bootlin.com/linux/v5.11.1/source/kernel/futex.c#L111>
        //   - `smp_mb()`: <https://lwn.net/Articles/847481/>
        self.counter.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = futex_wake(&self.counter, WakeWaiters::Amount(U31::new(1).unwrap())) {
            panic!("{e}");
        }
    }

    pub fn notify_all(&self) {
        if self.waiters.load(Ordering::Relaxed) == 0 {
            return;
        }
        self.counter.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = futex_wake(&self.counter, WakeWaiters::All) {
            panic!("{e}");
        }
    }
}
impl Default for CondVar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::*;

    #[test]
    fn test_cond_var() {
        let m = mutex::Mutex::new(0);
        // let m = std::sync::Mutex::new(0);
        let cv = CondVar::new();
        let mut wake_ups = 0;
        thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_secs(1));
                let mut m = m.lock();
                *m = 123;
                cv.notify_one();
            });
            let mut m = m.lock();
            while *m < 100 {
                m = cv.wait(m);
                wake_ups += 1;
            }
            assert_eq!(*m, 123);
        });

        // Check that the main thread actually did wait (not busy-loop),
        // while still allowing for a few spurious wake ups.
        assert!(wake_ups < 10);
    }
}
