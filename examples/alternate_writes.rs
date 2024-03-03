use std::{
    env::args,
    io,
    sync::atomic::{AtomicU32, Ordering},
};

use futex::{futex_wait, futex_wake, FutexWaitContext, WakeWaiters, U31};
use nix::{
    sys::wait::wait,
    unistd::{fork, ForkResult},
};
use rustix::mm::{mmap_anonymous, MapFlags, ProtFlags};

/// Based on the `futex_demo` on <https://lwn.net/Articles/638283/>.
pub fn main() {
    let n_loops = args().nth(1).map(|n| n.parse().unwrap()).unwrap_or(5);

    let i_addr = unsafe {
        mmap_anonymous(
            std::ptr::null_mut(),
            std::mem::size_of::<AtomicU32>() * 2,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
        )
    }
    .expect("mmap");
    let i_addr: *mut [AtomicU32; 2] = unsafe { std::mem::transmute(i_addr) };

    let futex_1 = unsafe { &(*i_addr)[0] };
    let futex_2 = unsafe { &(*i_addr)[1] };

    futex_1.store(FutexPState::Unavailable.into(), Ordering::SeqCst);
    futex_2.store(FutexPState::Available.into(), Ordering::SeqCst);

    let child_pid = unsafe { fork() }.expect("fork");
    match child_pid {
        ForkResult::Parent { .. } => {
            for j in 0..n_loops {
                f_wait(futex_2);
                let pid = std::process::id();
                println!("Parent  ({pid}) {j}");
                f_post(futex_1);
            }

            wait().unwrap();
        }
        ForkResult::Child => {
            for j in 0..n_loops {
                f_wait(futex_1);
                let pid = std::process::id();
                println!("Child  ({pid}) {j}");
                f_post(futex_2);
            }
        }
    }
}

enum FutexPState {
    Unavailable = 0,
    Available,
}
impl From<FutexPState> for u32 {
    fn from(value: FutexPState) -> Self {
        value as u32
    }
}

fn f_wait(futex_p: &AtomicU32) {
    loop {
        // Is the futex available?
        if futex_p
            .compare_exchange(
                FutexPState::Available.into(),
                FutexPState::Unavailable.into(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            // Yes
            break;
        }

        // Futex is not available; wait
        if let Err(e) = futex_wait(FutexWaitContext {
            word: futex_p,
            expected: FutexPState::Unavailable.into(),
            timeout: None,
        }) {
            if !matches!(e.kind(), io::ErrorKind::WouldBlock) {
                panic!("futex-FUTEX_WAIT");
            }
        }
    }
}

fn f_post(futex_p: &AtomicU32) {
    if futex_p
        .compare_exchange(
            FutexPState::Unavailable.into(),
            FutexPState::Available.into(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
    {
        if let Err(e) = futex_wake(futex_p, WakeWaiters::Amount(U31::new(1).unwrap())) {
            if !matches!(e.kind(), io::ErrorKind::WouldBlock) {
                panic!("futex-FUTEX_WAKE");
            }
        }
    }
}
