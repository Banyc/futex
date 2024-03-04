use std::{
    ops::{Deref, DerefMut},
    sync::atomic::AtomicU32,
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

/// # Panic
///
/// If `futex` is not in any of the [`State`].
pub fn lock(futex: &AtomicU32) {
    loop {
        // Assert `futex` is in valid state
        let _ = locked(futex);

        if futex
            .compare_exchange(
                State::Unlocked.into(),
                State::Locked.into(),
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            return;
        }
        match resumed_futex_wait(FutexWaitContext {
            word: futex,
            expected: State::Locked.into(),
            timeout: None,
        }) {
            Ok(()) => {
                continue;
            }
            Err(e) => match e.kind() {
                std::io::ErrorKind::WouldBlock => continue,
                _ => panic!("{e}"),
            },
        }
    }
}

/// # Panic
///
/// If `futex` is not in any of the [`State`].
pub fn unlock(futex: &AtomicU32) {
    if !locked(futex) {
        return;
    }
    futex.store(State::Unlocked.into(), std::sync::atomic::Ordering::SeqCst);
    futex_wake(futex, WakeWaiters::Amount(U31::new(1).unwrap())).unwrap();
}

/// # Panic
///
/// If `futex` is not in any of the [`State`].
fn locked(futex: &AtomicU32) -> bool {
    let s: State = futex
        .load(std::sync::atomic::Ordering::SeqCst)
        .try_into()
        .expect("unknown state");
    match s {
        State::Unlocked => false,
        State::Locked => true,
    }
}

pub struct Mutex<T> {
    futex: AtomicU32,
    value: SyncUnsafeCell<T>,
}
impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: SyncUnsafeCell::new(value),
            futex: new_unlocked_futex(),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        lock(&self.futex);
        MutexGuard { og: self }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

pub struct MutexGuard<'a, T> {
    og: &'a Mutex<T>,
}
impl<'a, T> MutexGuard<'a, T> {
    pub fn unlock(self) -> &'a Mutex<T> {
        unlock(&self.og.futex);
        self.og
    }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        unlock(&self.og.futex);
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
        unlock(&word);
    }

    #[test]
    fn test_lock_unlock() {
        let word = Arc::new(new_unlocked_futex());
        lock(&word);

        let waiting = std::thread::spawn({
            let word = word.clone();
            move || {
                lock(&word);
                unlock(&word);
            }
        });
        assert!(!waiting.is_finished());

        unlock(&word);

        waiting.join().unwrap();
    }
}
