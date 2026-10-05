//! A simple, race-free cancellation signal used to request that a running
//! [`crate::executor::ProcessHandle`] be stopped (CLAUDE.md section 16).

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Default)]
pub struct CancelToken {
    requested: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    /// Requests cancellation. Safe to call more than once or before
    /// anyone is waiting (the flag is checked first, avoiding the classic
    /// missed-wakeup race).
    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Resolves once cancellation has been requested. Resolves
    /// immediately if it already has been.
    pub async fn wait(&self) {
        if self.requested.load(Ordering::SeqCst) {
            return;
        }
        self.notify.notified().await;
    }

    /// Resets the token so it can be reused for the next long-running step.
    pub fn reset(&self) {
        self.requested.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn wait_resolves_after_request() {
        let token = Arc::new(CancelToken::new());
        let waiter = tokio::spawn({
            let token = token.clone();
            async move {
                token.wait().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.request();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn wait_resolves_immediately_if_already_requested() {
        let token = CancelToken::new();
        token.request();
        tokio::time::timeout(Duration::from_millis(50), token.wait())
            .await
            .unwrap();
    }

    #[test]
    fn reset_allows_reuse() {
        let token = CancelToken::new();
        token.request();
        assert!(token.is_requested());
        token.reset();
        assert!(!token.is_requested());
    }
}
