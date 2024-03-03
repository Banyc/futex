use std::{mem::transmute, sync::atomic::AtomicU32, time::Duration};

#[derive(Debug, Clone, Copy)]
pub struct FutexWaitContext<'a> {
    pub word: &'a AtomicU32,
    pub expected: u32,
    pub timeout: Option<(Duration, TimeoutMeasure)>,
}

/// # Behaviors
///
/// - If the futex word's value is not `expected`, it returns [`std::io::ErrorKind::WouldBlock`] error immediately
///   - to prevent lost wake-ups
/// - If the futex word's value matches `expected`, it sleeps until `FUTEX_WAKE` is called at `addr`
///
/// # Return
///
/// The return can be a spurious wake-up.
/// Therefore, callers should use the futex word's value to decide whether to continue to block or not.
pub fn futex_wait(cx: FutexWaitContext<'_>) -> std::io::Result<()> {
    let timeout_duration = cx.timeout.map(|(t, _m)| t);
    let measure = cx.timeout.map(|(_t, m)| m);
    let utime = timeout_duration.map(|t| {
        let tv_sec: i64 = unsafe { std::mem::transmute(t.as_secs()) };
        let tv_nsec = i64::from(t.subsec_nanos());
        rustix::thread::Timespec { tv_sec, tv_nsec }
    });
    let utime = match &utime {
        Some(utime) => utime as *const _,
        None => std::ptr::null(),
    };
    let flags = match measure {
        Some(TimeoutMeasure::RealTime) => rustix::thread::FutexFlags::CLOCK_REALTIME,
        None | Some(TimeoutMeasure::MonoTime) => rustix::thread::FutexFlags::empty(),
    };
    let ret = unsafe {
        rustix::thread::futex(
            cx.word.as_ptr(),
            rustix::thread::FutexOperation::Wait,
            flags,
            cx.expected,
            utime,
            std::ptr::null_mut(), // ignored
            0,                    // ignored
        )
    }?;
    assert_eq!(ret, 0);
    Ok(())
}
#[derive(Debug, Clone, Copy)]
pub enum TimeoutMeasure {
    RealTime,
    MonoTime,
}

/// Retry on spurious wake-ups and [`std::io::ErrorKind::Interrupted`].
///
/// Learn more from [`futex_wait`].
pub fn genuine_futex_wait(cx: FutexWaitContext<'_>) -> std::io::Result<()> {
    loop {
        let Err(e) = futex_wait(cx) else {
            if cx.word.load(std::sync::atomic::Ordering::Relaxed) == cx.expected {
                return Ok(());
            }
            // Spurious wake-up
            continue;
        };
        if matches!(e.kind(), std::io::ErrorKind::Interrupted) {
            continue;
        }
        return Err(e);
    }
}

/// Busy looping on [`std::io::ErrorKind::WouldBlock`].
pub fn busy_futex_wait(cx: FutexWaitContext<'_>) -> std::io::Result<()> {
    loop {
        let Err(e) = genuine_futex_wait(cx) else {
            return Ok(());
        };
        if matches!(e.kind(), std::io::ErrorKind::WouldBlock) {
            continue;
        }
        return Err(e);
    }
}

/// Returns the number of waiters that were woken up.
pub fn futex_wake(addr: &AtomicU32, waiters: WakeWaiters) -> std::io::Result<usize> {
    let waiters = match waiters {
        WakeWaiters::Amount(n) => n.get(),
        WakeWaiters::All => unsafe { transmute(i32::MAX) },
    };
    let woken_waiters = unsafe {
        rustix::thread::futex(
            addr.as_ptr(),
            rustix::thread::FutexOperation::Wake,
            rustix::thread::FutexFlags::empty(),
            waiters,
            std::ptr::null(),     // ignored
            std::ptr::null_mut(), // ignored
            0,                    // ignored
        )
    }?;
    Ok(woken_waiters)
}
#[derive(Debug, Clone, Copy)]
pub enum WakeWaiters {
    Amount(U31),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, std::hash::Hash)]
pub struct U31(u32);
impl U31 {
    pub fn new(v: u32) -> Option<U31> {
        if u32::try_from(i32::MAX).unwrap() < v {
            return None;
        }
        Some(Self(v))
    }

    pub fn get(&self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_wait_would_block() {
        let word = AtomicU32::new(0);
        let Err(e) = futex_wait(FutexWaitContext {
            word: &word,
            expected: 1,
            timeout: None,
        }) else {
            panic!();
        };
        assert!(matches!(e.kind(), std::io::ErrorKind::WouldBlock));
    }

    #[test]
    fn test_wake() {
        let word = Arc::new(AtomicU32::new(0));
        let waiter = std::thread::spawn({
            let word = word.clone();
            move || {
                futex_wait(FutexWaitContext {
                    word: &word.clone(),
                    expected: 0,
                    timeout: None,
                })
                .unwrap();
            }
        });
        loop {
            if futex_wake(&word, WakeWaiters::Amount(U31::new(1).unwrap())).unwrap() == 1 {
                break;
            }
        }
        waiter.join().unwrap();
    }
}
