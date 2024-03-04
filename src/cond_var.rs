use std::sync::atomic::AtomicU32;

use crate::{futex_wake, mutex, resumed_futex_wait, FutexWaitContext, WakeWaiters, U31};

pub struct CondVar {
    counter: AtomicU32,
}
impl CondVar {
    pub fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }

    /// Could be a spurious wake-up
    pub fn wait<'a, T>(&self, m: mutex::MutexGuard<'a, T>) -> mutex::MutexGuard<'a, T> {
        let c = self.counter.load(std::sync::atomic::Ordering::Relaxed);
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
        self.counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = futex_wake(&self.counter, WakeWaiters::Amount(U31::new(1).unwrap())) {
            panic!("{e}");
        }
    }

    pub fn notify_all(&self) {
        self.counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
