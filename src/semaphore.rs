use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{futex_wake, resumed_futex_wait, FutexWaitContext, WakeWaiters, U31};

/// A semaphore is an integer whose value is never allowed to fall below zero.
#[derive(Debug)]
pub struct Semaphore {
    value: AtomicU32,
    waiters: AtomicUsize,
}
impl Semaphore {
    pub fn new(value: u32) -> Self {
        Self {
            value: AtomicU32::new(value),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Decrement the semaphore value by one.
    /// If the semaphore value is currently zero, then it will block until the value becomes greater than zero.
    pub fn wait(&self) {
        loop {
            let value = self.value.load(Ordering::Relaxed);
            if 0 < value {
                if self
                    .value
                    .compare_exchange(value, value - 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            self.waiters.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = resumed_futex_wait(FutexWaitContext {
                word: &self.value,
                expected: 0,
                timeout: None,
            }) {
                if !matches!(e.kind(), std::io::ErrorKind::WouldBlock) {
                    panic!("{e}");
                }
            }
            self.waiters.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Increment the semaphore value by one.
    pub fn signal(&self) {
        loop {
            let waiters = self.waiters.load(Ordering::Relaxed);
            let value = self.value.load(Ordering::Relaxed);
            if self
                .value
                .compare_exchange(
                    value,
                    value.checked_add(1).expect("`u32` addition overflow"),
                    Ordering::Release,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                continue;
            }
            if 0 < waiters {
                futex_wake(&self.value, WakeWaiters::Amount(U31::new(1).unwrap())).unwrap();
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_wait_signal() {
        let sem = Semaphore::new(1);
        sem.wait();
        let sem = Arc::new(sem);
        let n = 10;
        let mut waiters = vec![];
        for i in 0..n {
            let waiter = std::thread::spawn({
                let sem = sem.clone();
                move || {
                    sem.wait();
                    dbg!(i);
                }
            });
            waiters.push(waiter);
        }

        for waiter in &waiters {
            assert!(!waiter.is_finished());
        }

        for _ in 0..waiters.len() {
            sem.signal();
        }

        for waiter in waiters.into_iter() {
            waiter.join().unwrap();
        }
    }
}
