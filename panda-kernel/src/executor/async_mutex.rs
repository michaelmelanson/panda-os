//! A simple async-aware mutex for use with the kernel's cooperative executor.
//!
//! Unlike a spinlock, this mutex can be held across `.await` points. When the
//! lock is contended, the waiting task yields (returns `Poll::Pending`) and
//! is woken when the lock becomes available.
//!
//! This is intentionally minimal — it supports only `lock()` and relies on
//! the guard's `Drop` to release. There is no `try_lock` or timeout.

use alloc::collections::VecDeque;
use core::cell::UnsafeCell;
use core::future::Future;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use spinning_top::Spinlock;

/// Internal state protected by a spinlock (only held briefly, never across awaits).
struct Inner<T> {
    locked: bool,
    waiters: VecDeque<Waker>,
    data: UnsafeCell<T>,
}

/// An async-aware mutex.
///
/// Safe to use across `.await` points in the kernel's cooperative executor.
/// The underlying data is only accessible through the returned `AsyncMutexGuard`.
pub struct AsyncMutex<T> {
    inner: Spinlock<Inner<T>>,
}

// Safety: AsyncMutex uses a Spinlock internally for synchronisation.
unsafe impl<T: Send> Send for AsyncMutex<T> {}
unsafe impl<T: Send> Sync for AsyncMutex<T> {}

impl<T> AsyncMutex<T> {
    /// Create a new unlocked `AsyncMutex` wrapping `value`.
    pub fn new(value: T) -> Self {
        Self {
            inner: Spinlock::new(Inner {
                locked: false,
                waiters: VecDeque::new(),
                data: UnsafeCell::new(value),
            }),
        }
    }

    /// Acquire the mutex, returning a guard that releases on drop.
    ///
    /// If the mutex is already held, the calling task yields until woken.
    pub fn lock(&self) -> AsyncMutexLock<'_, T> {
        AsyncMutexLock { mutex: self }
    }
}

/// Future returned by [`AsyncMutex::lock`].
pub struct AsyncMutexLock<'a, T> {
    mutex: &'a AsyncMutex<T>,
}

impl<'a, T> Future for AsyncMutexLock<'a, T> {
    type Output = AsyncMutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.mutex.inner.lock();
        if !inner.locked {
            inner.locked = true;
            Poll::Ready(AsyncMutexGuard { mutex: self.mutex })
        } else {
            // Register waker and yield
            inner.waiters.push_back(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// RAII guard that releases the [`AsyncMutex`] on drop.
pub struct AsyncMutexGuard<'a, T> {
    mutex: &'a AsyncMutex<T>,
}

impl<'a, T> Deref for AsyncMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // Safety: we hold the mutex, so exclusive access is guaranteed.
        unsafe { &*self.mutex.inner.lock().data.get() }
    }
}

impl<'a, T> DerefMut for AsyncMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        // Safety: we hold the mutex, so exclusive access is guaranteed.
        unsafe { &mut *self.mutex.inner.lock().data.get() }
    }
}

impl<'a, T> Drop for AsyncMutexGuard<'a, T> {
    fn drop(&mut self) {
        let mut inner = self.mutex.inner.lock();
        inner.locked = false;
        // Wake the next waiter, if any
        if let Some(waker) = inner.waiters.pop_front() {
            waker.wake();
        }
    }
}
