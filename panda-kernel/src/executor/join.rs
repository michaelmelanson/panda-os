//! Combinators for running multiple futures concurrently.
//!
//! These are useful for issuing independent I/O operations in parallel so that
//! both can make progress when the executor polls the parent task.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Future that polls two fallible futures concurrently, returning when both
/// complete or when the first error is encountered.
///
/// Both futures are polled each time the combinator is polled, which allows
/// the underlying I/O operations to be issued in parallel rather than
/// sequentially.
///
/// # Safety note on pinning
///
/// The inner futures are structurally pinned: once `TryJoin` is pinned, the
/// contained futures are never moved. This is safe because we only access
/// them through `Pin::new_unchecked` after the outer `Pin` guarantees
/// immovability.
pub struct TryJoin<A, B, T1, T2, E>
where
    A: Future<Output = Result<T1, E>>,
    B: Future<Output = Result<T2, E>>,
{
    a: MaybeDone<A, T1, E>,
    b: MaybeDone<B, T2, E>,
}

enum MaybeDone<F: Future<Output = Result<T, E>>, T, E> {
    Pending(F),
    Done(T),
    Taken,
}

impl<A, B, T1, T2, E> TryJoin<A, B, T1, T2, E>
where
    A: Future<Output = Result<T1, E>>,
    B: Future<Output = Result<T2, E>>,
{
    fn new(a: A, b: B) -> Self {
        Self {
            a: MaybeDone::Pending(a),
            b: MaybeDone::Pending(b),
        }
    }
}

impl<A, B, T1, T2, E> Future for TryJoin<A, B, T1, T2, E>
where
    A: Future<Output = Result<T1, E>>,
    B: Future<Output = Result<T2, E>>,
{
    type Output = Result<(T1, T2), E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We perform structural pinning — the inner futures are never
        // moved once TryJoin is pinned. We only project through Pin below.
        let this = unsafe { self.get_unchecked_mut() };

        // Poll A if still pending
        if let MaybeDone::Pending(ref mut fut) = this.a {
            // Safety: fut is structurally pinned within self
            match unsafe { Pin::new_unchecked(fut) }.poll(cx) {
                Poll::Ready(Ok(val)) => this.a = MaybeDone::Done(val),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {}
            }
        }

        // Poll B if still pending
        if let MaybeDone::Pending(ref mut fut) = this.b {
            // Safety: fut is structurally pinned within self
            match unsafe { Pin::new_unchecked(fut) }.poll(cx) {
                Poll::Ready(Ok(val)) => this.b = MaybeDone::Done(val),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {}
            }
        }

        // Check if both are done
        match (&this.a, &this.b) {
            (MaybeDone::Done(_), MaybeDone::Done(_)) => {
                let a = core::mem::replace(&mut this.a, MaybeDone::Taken);
                let b = core::mem::replace(&mut this.b, MaybeDone::Taken);
                match (a, b) {
                    (MaybeDone::Done(a), MaybeDone::Done(b)) => Poll::Ready(Ok((a, b))),
                    _ => unreachable!(),
                }
            }
            _ => Poll::Pending,
        }
    }
}

/// Run two fallible futures concurrently, returning when both complete
/// successfully or when the first error is encountered.
///
/// This allows independent I/O operations (e.g., writing the superblock and
/// a block group descriptor) to be issued in parallel.
///
/// # Example
///
/// ```ignore
/// try_join(
///     self.write_block_group_descriptor(group),
///     self.write_superblock(),
/// ).await?;
/// ```
pub fn try_join<A, B, T1, T2, E>(a: A, b: B) -> TryJoin<A, B, T1, T2, E>
where
    A: Future<Output = Result<T1, E>>,
    B: Future<Output = Result<T2, E>>,
{
    TryJoin::new(a, b)
}
