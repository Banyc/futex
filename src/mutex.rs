use std::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU32, AtomicUsize},
};

use sync_unsafe_cell::SyncUnsafeCell;

use crate::{futex_wake, resumed_futex_wait, FutexWaitContext, WakeWaiters, U31};

#[derive(Debug, Clone, Copy)]
pub enum State {
    Unlocked = 0,
    Locked,
}
impl From<State> for u32 {
    fn from(value: State) -> Self {
        value as u32
    }
}
impl TryFrom<u32> for State {
    type Error = ();
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == State::Locked.into() {
            return Ok(State::Locked);
        }
        if value == State::Unlocked.into() {
            return Ok(State::Unlocked);
        }
        Err(())
    }
}

pub fn new_unlocked_futex() -> AtomicU32 {
    AtomicU32::new(State::Unlocked.into())
}

/// Return `false` if it fails to lock in a nonblocking setting.
///
/// # Panic
///
/// If `futex` is not in any of the [`State`].
pub fn lock(futex: &AtomicU32, waiters: Option<&AtomicUsize>, blocking: LockBlocking) -> bool {
    const RETRIES: usize = 128;
    loop {
        // Assert `futex` is in valid state
        let _ = locked(futex);

        for _ in 0..RETRIES {
            if futex
                .compare_exchange(
                    State::Unlocked.into(),
                    State::Locked.into(),
                    std::sync::atomic::Ordering::Acquire,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                return true;
            }
            std::hint::spin_loop();
        }
        match blocking {
            LockBlocking::Blocking => {
                if let Some(waiters) = waiters {
                    waiters.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if let Err(e) = resumed_futex_wait(FutexWaitContext {
                    word: futex,
                    expected: State::Locked.into(),
                    timeout: None,
                }) {
                    if !matches!(e.kind(), std::io::ErrorKind::WouldBlock) {
                        panic!("{e}");
                    }
                }
                if let Some(waiters) = waiters {
                    waiters.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            LockBlocking::Nonblocking => {
                return false;
            }
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum LockBlocking {
    Blocking,
    Nonblocking,
}

/// # Panic
///
/// If `futex` is not in any of the [`State`].
pub fn unlock(futex: &AtomicU32, waiters: Option<&AtomicUsize>) {
    if !locked(futex) {
        return;
    }
    futex.store(State::Unlocked.into(), std::sync::atomic::Ordering::Relaxed);
    if let Some(waiters) = waiters {
        if 0 == waiters.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
    }
    futex_wake(futex, WakeWaiters::Amount(U31::new(1).unwrap())).unwrap();
}

/// # Panic
///
/// If `futex` is not in any of the [`State`].
fn locked(futex: &AtomicU32) -> bool {
    let s: State = futex
        .load(std::sync::atomic::Ordering::Relaxed)
        .try_into()
        .expect("unknown state");
    match s {
        State::Unlocked => false,
        State::Locked => true,
    }
}

pub struct Mutex<T> {
    futex: AtomicU32,
    waiters: AtomicUsize,
    value: SyncUnsafeCell<T>,
}
impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: SyncUnsafeCell::new(value),
            waiters: AtomicUsize::new(0),
            futex: new_unlocked_futex(),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        lock(&self.futex, Some(&self.waiters), LockBlocking::Blocking);
        MutexGuard { og: self }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if !lock(&self.futex, Some(&self.waiters), LockBlocking::Nonblocking) {
            return None;
        };
        Some(MutexGuard { og: self })
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}
impl<T: core::fmt::Debug> core::fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mutex")
            .field("futex", &self.futex)
            .field("value", &unsafe { self.value.get().as_ref() })
            .finish()
    }
}

pub struct MutexGuard<'a, T> {
    og: &'a Mutex<T>,
}
impl<'a, T> MutexGuard<'a, T> {
    pub fn unlock(self) -> &'a Mutex<T> {
        unlock(&self.og.futex, Some(&self.og.waiters));
        self.og
    }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        unlock(&self.og.futex, Some(&self.og.waiters));
    }
}
impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.og.value.get().as_ref() }.unwrap()
    }
}
impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.og.value.get().as_mut() }.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_unlock() {
        let word = new_unlocked_futex();
        unlock(&word, None);
    }

    #[test]
    fn test_lock_unlock() {
        let word = Arc::new(new_unlocked_futex());
        lock(&word, None, LockBlocking::Blocking);

        let waiting = std::thread::spawn({
            let word = word.clone();
            move || {
                lock(&word, None, LockBlocking::Blocking);
                unlock(&word, None);
            }
        });
        assert!(!waiting.is_finished());

        unlock(&word, None);

        waiting.join().unwrap();
    }
}
