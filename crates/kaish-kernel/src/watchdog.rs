//! Movable-deadline watchdog for the per-execute timeout.
//!
//! `execute_with_options` used to spawn a timer that slept the whole script
//! budget and fired the cancel token — nothing could suspend or extend it
//! mid-script. That can't serve model-backed builtins (provider calls that
//! legitimately run minutes): stretching the *script* budget to minutes hands
//! a `while true` loop the same minutes. The two jobs need separate knobs.
//!
//! [`Watchdog`] keeps the deadline in a `tokio::sync::watch` channel the timer
//! task re-arms against. Builtins suspend the script clock through
//! [`ToolCtx::patient`](kaish_tool_api::ToolCtx::patient), which acquires a
//! [`WatchdogHold`]: while held, the hold's own budget governs the deadline;
//! on drop the script clock resumes with the remaining time it had at acquire
//! (the same RAII discipline as the kernel's `VarsFrameGuard`/`CwdGuard`).
//!
//! Only the timer is suspendable. The cancel token the watchdog fires is the
//! same one `Kernel::cancel()` and the embedder token cascade into — those
//! stay live during a hold.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Far-enough future for budgets too large to add to `Instant` without
/// overflow (e.g. `Duration::MAX`): one year of patience.
fn deadline_after(now: Instant, budget: Duration) -> Instant {
    now.checked_add(budget)
        .unwrap_or_else(|| now + Duration::from_secs(60 * 60 * 24 * 365))
}

struct State {
    /// Deadline of the active (innermost) regime — mirrored into `deadline_tx`
    /// under the same lock so the timer task never sees a stale value.
    deadline: Instant,
    /// Regimes suspended by patient holds, bottom-up. Each entry is
    /// `(id of the hold that suspended it, frozen remaining time of the
    /// regime below)`; entry 0 froze the script budget. LIFO in the common
    /// case, but out-of-order release (a hold moved across tasks) is handled.
    suspended: Vec<(u64, Duration)>,
    next_id: u64,
}

/// The per-execute timeout timer with a movable deadline.
///
/// Created by the kernel when a script timeout is in effect, shared with
/// every `ExecContext` snapshot of that execution (and its forks) so builtins
/// can acquire patient holds against it.
pub struct Watchdog {
    state: Mutex<State>,
    deadline_tx: watch::Sender<Instant>,
}

impl Watchdog {
    /// Start the clock: the script deadline is `budget` from now.
    pub fn new(budget: Duration) -> Self {
        let deadline = deadline_after(Instant::now(), budget);
        let (deadline_tx, _) = watch::channel(deadline);
        Self {
            state: Mutex::new(State { deadline, suspended: Vec::new(), next_id: 0 }),
            deadline_tx,
        }
    }

    /// Timer task body: sleep toward the current deadline, re-arming whenever
    /// a patient hold moves it. When the deadline is genuinely reached, set
    /// `elapsed` (so the kernel maps the result to exit 124) and fire `token`.
    pub async fn run(self: Arc<Self>, elapsed: Arc<AtomicBool>, token: CancellationToken) {
        let mut deadline_rx = self.deadline_tx.subscribe();
        loop {
            let deadline = *deadline_rx.borrow_and_update();
            if Instant::now() >= deadline {
                elapsed.store(true, Ordering::SeqCst);
                token.cancel();
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {}
                // Sender lives in self, which we own — changed() can't fail,
                // and a send between borrow_and_update and here wakes us.
                _ = deadline_rx.changed() => {}
            }
        }
    }

    /// Suspend the active regime: freeze its remaining time and let `budget`
    /// govern the deadline until the hold drops.
    pub fn hold(self: &Arc<Self>, budget: Duration) -> WatchdogHold {
        #[allow(clippy::expect_used)]
        let mut state = self.state.lock().expect("watchdog state poisoned");
        let now = Instant::now();
        let id = state.next_id;
        state.next_id += 1;
        // tokio's duration_since saturates to zero, so a hold acquired after
        // the deadline passed (watchdog already fired) restores to "expired".
        let remaining = state.deadline.duration_since(now);
        state.suspended.push((id, remaining));
        state.deadline = deadline_after(now, budget);
        self.deadline_tx.send_replace(state.deadline);
        drop(state);
        WatchdogHold { watchdog: self.clone(), id }
    }

    fn release(&self, id: u64) {
        #[allow(clippy::expect_used)]
        let mut state = self.state.lock().expect("watchdog state poisoned");
        let Some(index) = state.suspended.iter().position(|(hold_id, _)| *hold_id == id) else {
            // Double release is unreachable (RAII, ids are unique); ignore.
            return;
        };
        let (_, saved) = state.suspended.remove(index);
        if index == state.suspended.len() {
            // The released hold owned the active regime: resume the regime it
            // froze, with the remaining time it had at acquire.
            state.deadline = deadline_after(Instant::now(), saved);
            self.deadline_tx.send_replace(state.deadline);
        } else {
            // Out-of-order release: the hold directly above (now at `index`)
            // had frozen *this* hold's remaining; it inherits our parent's
            // remaining instead, and the active deadline is untouched.
            state.suspended[index].1 = saved;
        }
    }
}

/// RAII hold on a [`Watchdog`]: releases (and restores the frozen remaining
/// time) on drop. Handed to tools boxed inside a
/// [`PatientGuard`](kaish_tool_api::PatientGuard).
pub struct WatchdogHold {
    watchdog: Arc<Watchdog>,
    id: u64,
}

impl Drop for WatchdogHold {
    fn drop(&mut self) {
        self.watchdog.release(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Let the spawned timer task observe advanced time / channel updates.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn spawn_watchdog(
        budget: Duration,
    ) -> (Arc<Watchdog>, Arc<AtomicBool>, CancellationToken, tokio::task::JoinHandle<()>) {
        let watchdog = Arc::new(Watchdog::new(budget));
        let elapsed = Arc::new(AtomicBool::new(false));
        let token = CancellationToken::new();
        let handle = tokio::spawn(watchdog.clone().run(elapsed.clone(), token.clone()));
        (watchdog, elapsed, token, handle)
    }

    #[tokio::test(start_paused = true)]
    async fn fires_at_deadline() {
        let (_watchdog, elapsed, token, handle) = spawn_watchdog(Duration::from_secs(1));
        settle().await;
        tokio::time::advance(Duration::from_millis(999)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "fired before the deadline");
        tokio::time::advance(Duration::from_millis(2)).await;
        handle.await.expect("timer task");
        assert!(elapsed.load(Ordering::SeqCst));
        assert!(token.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn hold_freezes_script_clock_and_restores_remaining() {
        let (watchdog, elapsed, _token, handle) = spawn_watchdog(Duration::from_secs(1));
        settle().await;
        tokio::time::advance(Duration::from_millis(400)).await;
        settle().await;

        // 600ms of script budget remain; freeze them under a 10s hold.
        let hold = watchdog.hold(Duration::from_secs(10));
        tokio::time::advance(Duration::from_secs(5)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "fired while the script clock was frozen");

        // Drop resumes the script clock with the frozen 600ms.
        drop(hold);
        tokio::time::advance(Duration::from_millis(599)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "restored remaining was shortened");
        tokio::time::advance(Duration::from_millis(2)).await;
        handle.await.expect("timer task");
        assert!(elapsed.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn hold_budget_overrun_fires() {
        let (watchdog, elapsed, token, handle) = spawn_watchdog(Duration::from_secs(60));
        settle().await;
        let _hold = watchdog.hold(Duration::from_millis(500));
        tokio::time::advance(Duration::from_millis(501)).await;
        handle.await.expect("timer task");
        assert!(elapsed.load(Ordering::SeqCst), "hold overran its budget but didn't fire");
        assert!(token.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn nested_holds_restore_in_lifo_order() {
        let (watchdog, elapsed, _token, handle) = spawn_watchdog(Duration::from_secs(1));
        settle().await;

        let outer = watchdog.hold(Duration::from_secs(10));
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        // 8s of outer budget remain; freeze under the inner hold.
        let inner = watchdog.hold(Duration::from_secs(30));
        tokio::time::advance(Duration::from_secs(20)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst));

        // Inner drop resumes the outer hold's 8s; outer drop resumes the
        // script's full 1s (frozen before any time passed under it).
        drop(inner);
        tokio::time::advance(Duration::from_millis(7_999)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "outer remaining was shortened");
        drop(outer);
        tokio::time::advance(Duration::from_millis(999)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "script remaining was shortened");
        tokio::time::advance(Duration::from_millis(2)).await;
        handle.await.expect("timer task");
        assert!(elapsed.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn out_of_order_release_keeps_chain_consistent() {
        let (watchdog, elapsed, _token, handle) = spawn_watchdog(Duration::from_secs(1));
        settle().await;

        let first = watchdog.hold(Duration::from_secs(10));
        let second = watchdog.hold(Duration::from_secs(30));
        // Release the *first* hold while the second is still active: the
        // second's regime keeps running, and its eventual drop must restore
        // the script's remaining (not the gone first hold's).
        drop(first);
        tokio::time::advance(Duration::from_secs(20)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "second hold's budget was lost");
        drop(second);
        tokio::time::advance(Duration::from_millis(999)).await;
        settle().await;
        assert!(!elapsed.load(Ordering::SeqCst), "script remaining was lost");
        tokio::time::advance(Duration::from_millis(2)).await;
        handle.await.expect("timer task");
        assert!(elapsed.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn hold_acquired_after_fire_is_harmless() {
        let (watchdog, elapsed, _token, handle) = spawn_watchdog(Duration::from_millis(10));
        tokio::time::advance(Duration::from_millis(11)).await;
        handle.await.expect("timer task");
        assert!(elapsed.load(Ordering::SeqCst));
        // The timer task is gone; acquiring and dropping a hold is a no-op.
        let hold = watchdog.hold(Duration::from_secs(5));
        drop(hold);
    }
}
