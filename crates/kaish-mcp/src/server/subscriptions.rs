//! Resource subscription tracking for MCP.
//!
//! Tracks which resource URIs clients have subscribed to, enabling
//! `notifications/resources/updated` when subscribed resources change.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Tracks active resource subscriptions.
///
/// Thread-safe via internal `RwLock`. Designed for single-client stdio
/// transport (one subscriber set). For multi-client HTTP transport,
/// extend with per-session tracking.
#[derive(Debug, Default)]
pub struct SubscriptionTracker {
    subscribed_uris: RwLock<HashSet<String>>,
}

impl SubscriptionTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            subscribed_uris: RwLock::new(HashSet::new()),
        })
    }

    /// Subscribe to updates for a resource URI.
    pub async fn subscribe(&self, uri: String) {
        self.subscribed_uris.write().await.insert(uri);
    }

    /// Unsubscribe from updates for a resource URI.
    pub async fn unsubscribe(&self, uri: &str) {
        self.subscribed_uris.write().await.remove(uri);
    }

    /// Check if a URI has active subscriptions.
    pub async fn is_subscribed(&self, uri: &str) -> bool {
        self.subscribed_uris.read().await.contains(uri)
    }

    /// Get all currently subscribed URIs.
    pub async fn subscribed_uris(&self) -> Vec<String> {
        self.subscribed_uris.read().await.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_subscribe_unsubscribe() {
        let tracker = SubscriptionTracker::new();

        tracker.subscribe("kaish://vfs/tmp".to_string()).await;
        assert!(tracker.is_subscribed("kaish://vfs/tmp").await);

        tracker.unsubscribe("kaish://vfs/tmp").await;
        assert!(!tracker.is_subscribed("kaish://vfs/tmp").await);
    }

    #[tokio::test]
    async fn test_multiple_subscriptions() {
        let tracker = SubscriptionTracker::new();

        tracker.subscribe("kaish://vfs/a".to_string()).await;
        tracker.subscribe("kaish://vfs/b".to_string()).await;

        let uris = tracker.subscribed_uris().await;
        assert_eq!(uris.len(), 2);
        assert!(uris.contains(&"kaish://vfs/a".to_string()));
        assert!(uris.contains(&"kaish://vfs/b".to_string()));
    }

    #[tokio::test]
    async fn test_unsubscribe_nonexistent() {
        let tracker = SubscriptionTracker::new();
        // Should not panic
        tracker.unsubscribe("kaish://vfs/nonexistent").await;
    }

    #[tokio::test]
    async fn test_duplicate_subscribe() {
        let tracker = SubscriptionTracker::new();

        tracker.subscribe("kaish://vfs/a".to_string()).await;
        tracker.subscribe("kaish://vfs/a".to_string()).await;

        let uris = tracker.subscribed_uris().await;
        assert_eq!(uris.len(), 1);
    }
}
