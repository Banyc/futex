use std::sync::atomic::AtomicU32;

use crate::{futex_wake, resumed_futex_wait, FutexWaitContext, WakeWaiters, U31};

const EXPECTED_FUTEX: u32 = 0;

/// A semaphore is an integer whose value is never allowed to fall below zero.
#[derive(Debug)]
pub struct Semaphore {
    futex: AtomicU32,
    value: AtomicU32,
}
impl Semaphore {
    pub fn new(value: u32) -> Self {
        Self {
            futex: AtomicU32::new(EXPECTED_FUTEX),
            value: AtomicU32::new(value),
        }
    }

    /// Decrement the semaphore value by one.
    /// If the semaphore value is currently zero, then it will block until the value becomes greater than zero.
    pub fn wait(&self) {
        loop {
            let value = self.value.load(std::sync::atomic::Ordering::SeqCst);
            if 0 < value {
                if self
                    .value
                    .compare_exchange(
                        value,
                        value - 1,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            resumed_futex_wait(FutexWaitContext {
                word: &self.futex,
                expected: EXPECTED_FUTEX,
                timeout: None,
            })
            .unwrap();
        }
    }

    /// Increment the semaphore value by one.
    pub fn signal(&self) {
        loop {
            let value = self.value.load(std::sync::atomic::Ordering::SeqCst);
            if self
                .value
                .compare_exchange(
                    value,
                    value.checked_add(1).expect("`u32` addition overflow"),
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
            {
                if value == 0 {
                    futex_wake(&self.futex, WakeWaiters::Amount(U31::new(1).unwrap())).unwrap();
                }
                break;
            }
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
        for _ in 0..n {
            let waiter = std::thread::spawn({
                let sem = sem.clone();
                move || {
                    sem.wait();
                }
            });
            waiters.push(waiter);
        }

        for waiter in &waiters {
            assert!(!waiter.is_finished());
        }

        for i in 0..waiters.len() {
            assert_eq!(waiters.iter().filter(|w| w.is_finished()).count(), i);
            sem.signal();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        for waiter in waiters {
            waiter.join().unwrap();
        }
    }
}
