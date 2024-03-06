use std::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{cond_var, mutex};

/// Multiple writers; single reader.
#[derive(Debug)]
pub struct RingBuffer<T, const N: usize> {
    buf: [Cell<T>; N],
    /// Points to the next cell to read.
    /// The pointed cell is unavailable if `write_ptr` is also pointing to the same one.
    ///
    /// Only allowed to go forward, never backward.
    ///
    /// Never proactively surpass `write_ptr`.
    ///
    /// Only increment when the current cell has been properly modified.
    read_ptr: AtomicUsize,
    /// Points to the next cell to write
    ///     
    /// Only allowed to go forward, never backward.
    ///
    /// Never proactively share cells with `read_ptr`.
    ///
    /// Only increment when the current cell has been properly modified.
    write_ptr: AtomicUsize,
}
impl<T, const N: usize> RingBuffer<T, N> {
    /// # Panic
    ///
    /// If `N` does not reach `2`.
    pub fn new() -> Self {
        assert!(3 <= N);
        assert!(N != usize::MAX);
        let buf = {
            let mut buf: [MaybeUninit<Cell<T>>; N] = unsafe { MaybeUninit::uninit().assume_init() };
            for cell in buf.iter_mut() {
                *cell = MaybeUninit::new(Cell::new());
            }
            unsafe { std::mem::transmute_copy::<_, [Cell<T>; N]>(&buf) }
        };
        Self {
            buf,
            read_ptr: AtomicUsize::new(0),
            write_ptr: AtomicUsize::new(0),
        }
    }

    fn positive_distance(&self, src: usize, dst: usize) -> usize {
        if src <= dst {
            return dst - src;
        };
        let new_src = src - dst;
        let new_dst = self.buf.len();
        new_dst - new_src
    }

    pub fn write_override(&self, new: T) {
        let mut new = Some(new);
        while new.is_some() {
            // Override
            let write_ptr = loop {
                let write_ptr = self.write_ptr.load(Ordering::SeqCst);
                let read_ptr = self.read_ptr.load(Ordering::SeqCst);

                if self.positive_distance(write_ptr, read_ptr) == 1 {
                    let cell = &self.buf[read_ptr];
                    let mut m = cell.write();
                    if read_ptr != self.read_ptr.load(Ordering::SeqCst) {
                        continue;
                    };
                    *m.mutex().deref_mut() = CellValue::Cancelled;

                    if self
                        .read_ptr
                        .compare_exchange(
                            read_ptr,
                            (read_ptr + 1) % self.buf.len(),
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_err()
                    {
                        continue;
                    }
                }
                break write_ptr;
            };

            let cell = &self.buf[write_ptr];
            let mut m = cell.write();
            if write_ptr != self.write_ptr.load(Ordering::SeqCst) {
                continue;
            }
            **m.mutex() = CellValue::Some(new.take().unwrap());
            self.write_ptr
                .compare_exchange(
                    write_ptr,
                    (write_ptr + 1) % self.buf.len(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .unwrap();
        }
    }

    pub fn read(&self) -> T {
        loop {
            let read_ptr = self.read_ptr.load(Ordering::SeqCst);
            let cell = &self.buf[read_ptr];
            let m = cell.read(|| read_ptr == self.read_ptr.load(Ordering::SeqCst));
            let Some(mut m) = m else {
                continue;
            };
            let write_ptr = self.write_ptr.load(Ordering::SeqCst);
            if read_ptr == write_ptr {
                continue;
            }
            let read = m.take().unwrap();
            if self
                .read_ptr
                .compare_exchange(
                    read_ptr,
                    (read_ptr + 1) % self.buf.len(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                continue;
            }
            return read;
        }
    }
}
impl<T, const N: usize> Default for RingBuffer<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DebugRingBuffer<T: core::fmt::Debug, const N: usize>(pub RingBuffer<T, N>);
impl<T: core::fmt::Debug, const N: usize> DebugRingBuffer<T, N> {
    pub fn get(&self) -> &RingBuffer<T, N> {
        &self.0
    }
}
impl<T: core::fmt::Debug, const N: usize> Drop for DebugRingBuffer<T, N> {
    fn drop(&mut self) {
        dbg!(&self.0);
    }
}

#[derive(Debug)]
struct Cell<T> {
    cond_var: cond_var::CondVar,
    mutex: mutex::Mutex<CellValue<T>>,
}
impl<T> Cell<T> {
    pub fn new() -> Self {
        Self {
            cond_var: cond_var::CondVar::new(),
            mutex: mutex::Mutex::new(CellValue::Vacant),
        }
    }

    /// Instructions in the `new` closure are protected by a mutex
    pub fn write(&self) -> WriteGuard<'_, T> {
        let m = self.mutex.lock();
        WriteGuard {
            mutex: m,
            cond_var: &self.cond_var,
        }
    }

    /// Instructions in the `confirmed` closure are protected by a mutex
    pub fn read(
        &self,
        mut confirmed: impl FnMut() -> bool,
    ) -> Option<mutex::MutexGuard<'_, CellValue<T>>> {
        let mut m = self.mutex.lock();
        if !confirmed() {
            return None;
        }
        loop {
            match m.deref() {
                CellValue::Some(_) => {
                    return Some(m);
                }
                CellValue::Vacant => {
                    m = self.cond_var.wait(m);
                }
                CellValue::Cancelled => {
                    return None;
                }
            }
        }
    }
}
impl<T> Default for Cell<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum CellValue<T> {
    Vacant,
    Some(T),
    Cancelled,
}
impl<T> CellValue<T> {
    pub fn take(&mut self) -> Option<T> {
        match self {
            CellValue::Vacant => return None,
            CellValue::Some(_) => (),
            CellValue::Cancelled => return None,
        }
        let a = std::mem::replace(self, Self::Vacant);
        match a {
            CellValue::Vacant => unreachable!(),
            CellValue::Some(v) => Some(v),
            CellValue::Cancelled => unreachable!(),
        }
    }

    pub fn is_vacant(&self) -> bool {
        match self {
            CellValue::Vacant => true,
            CellValue::Some(_) => false,
            CellValue::Cancelled => false,
        }
    }
}

pub struct WriteGuard<'a, T> {
    mutex: mutex::MutexGuard<'a, CellValue<T>>,
    cond_var: &'a cond_var::CondVar,
}
impl<'a, T> WriteGuard<'a, T> {
    pub fn mutex(&mut self) -> &mut mutex::MutexGuard<'a, CellValue<T>> {
        &mut self.mutex
    }
}
impl<T> Drop for WriteGuard<'_, T> {
    fn drop(&mut self) {
        self.cond_var.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_1() {
        const BUF_SIZE: usize = 3;
        let ring_buf: RingBuffer<usize, BUF_SIZE> = RingBuffer::new();
        let ring_buf = Arc::new(DebugRingBuffer(ring_buf));
        let writes = u16::MAX as usize;

        ctrlc::set_handler({
            let ring_buf = ring_buf.clone();
            move || {
                dbg!(&ring_buf.get());
            }
        })
        .unwrap();

        std::thread::scope(|s| {
            // Reader
            s.spawn({
                let ring_buf = ring_buf.clone();
                move || {
                    let mut prev = writes;
                    loop {
                        let n = ring_buf.get().read();
                        dbg!(n);
                        assert!(n < prev);
                        if n == 0 {
                            return;
                        }
                        prev = n;
                    }
                }
            });

            // Writer
            s.spawn({
                let ring_buf = ring_buf.clone();
                move || {
                    for i in (0..writes).rev() {
                        ring_buf.get().write_override(i);
                    }
                }
            });
        });
    }
}
