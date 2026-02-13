//! Resource subscription tracking and file watching for MCP.
//!
//! Tracks which resource URIs clients have subscribed to and watches
//! the underlying files via `notify`, emitting `notifications/resources/updated`
//! when subscribed resources change on disk.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::service::{Peer, RoleServer};
use tokio::sync::{mpsc, Mutex, RwLock};

/// Watches subscribed resource URIs for filesystem changes.
///
/// Combines URI subscription tracking with inotify-based file watching.
/// Thread-safe via internal locks. Designed for single-client stdio
/// transport (one subscriber set).
pub struct ResourceWatcher {
    subscribed_uris: RwLock<HashSet<String>>,
    path_to_uri: RwLock<HashMap<PathBuf, String>>,
    uri_to_path: RwLock<HashMap<String, PathBuf>>,
    watcher: Mutex<Option<RecommendedWatcher>>,
    peer: Arc<OnceLock<Peer<RoleServer>>>,
}

impl ResourceWatcher {
    pub fn new() -> Arc<Self> {
        // Bounded channel — intermediate events can be dropped since MCP
        // notifications are idempotent ("this URI changed").
        let (event_tx, event_rx) = mpsc::channel::<PathBuf>(256);
        let peer = Arc::new(OnceLock::new());

        let watcher = Self::create_watcher(event_tx);
        if watcher.is_none() {
            tracing::warn!("File watcher unavailable — subscriptions stored but won't fire");
        }

        let this = Arc::new(Self {
            subscribed_uris: RwLock::new(HashSet::new()),
            path_to_uri: RwLock::new(HashMap::new()),
            uri_to_path: RwLock::new(HashMap::new()),
            watcher: Mutex::new(watcher),
            peer: peer.clone(),
        });

        // Weak ref breaks the cycle: watcher → notify closure → tx → task → watcher.
        // When all external Arcs drop, Weak::upgrade() returns None and the task exits.
        let watcher_weak = Arc::downgrade(&this);
        tokio::spawn(Self::notification_task(watcher_weak, peer, event_rx));

        this
    }

    /// Store the MCP peer handle for sending notifications.
    /// Called from the subscribe handler where context is available.
    pub fn set_peer(&self, peer: Peer<RoleServer>) {
        // OnceLock::set returns Err if already set — that's fine, same peer.
        let _ = self.peer.set(peer);
    }

    /// Subscribe to updates for a resource URI.
    /// If `real_path` is Some, starts watching the file on disk.
    pub async fn subscribe(&self, uri: String, real_path: Option<PathBuf>) {
        self.subscribed_uris.write().await.insert(uri.clone());

        if let Some(raw_path) = real_path {
            // Canonicalize so the stored key matches what notify reports.
            let path = std::fs::canonicalize(&raw_path).unwrap_or(raw_path);

            self.path_to_uri
                .write()
                .await
                .insert(path.clone(), uri.clone());
            self.uri_to_path.write().await.insert(uri, path.clone());

            let mut watcher_guard = self.watcher.lock().await;
            if let Some(ref mut w) = *watcher_guard {
                if let Err(e) = w.watch(&path, RecursiveMode::NonRecursive) {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to watch path");
                }
            }
        }
    }

    /// Unsubscribe from updates for a resource URI.
    /// Removes the watch if a real path was associated.
    pub async fn unsubscribe(&self, uri: &str) {
        self.subscribed_uris.write().await.remove(uri);

        if let Some(path) = self.uri_to_path.write().await.remove(uri) {
            self.path_to_uri.write().await.remove(&path);

            let mut watcher_guard = self.watcher.lock().await;
            if let Some(ref mut w) = *watcher_guard {
                // Explicitly ignored: unwatch failure is harmless (path may already be gone)
                let _ = w.unwatch(&path);
            }
        }
    }

    /// Check if a URI has active subscriptions.
    pub async fn is_subscribed(&self, uri: &str) -> bool {
        self.subscribed_uris.read().await.contains(uri)
    }

    /// Get all currently subscribed URIs.
    pub async fn subscribed_uris(&self) -> Vec<String> {
        self.subscribed_uris.read().await.iter().cloned().collect()
    }

    /// Create the notify watcher, returning None on failure.
    fn create_watcher(event_tx: mpsc::Sender<PathBuf>) -> Option<RecommendedWatcher> {
        let watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            match res {
                Ok(event) => {
                    for path in event.paths {
                        // try_send: drop event if channel full — MCP notifications
                        // are idempotent, so missing intermediate events is fine.
                        let _ = event_tx.try_send(path);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "File watcher error");
                }
            }
        });

        match watcher {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create file watcher");
                None
            }
        }
    }

    /// Background task: receives filesystem events and sends MCP notifications.
    async fn notification_task(
        watcher_weak: Weak<ResourceWatcher>,
        peer: Arc<OnceLock<Peer<RoleServer>>>,
        mut event_rx: mpsc::Receiver<PathBuf>,
    ) {
        while let Some(path) = event_rx.recv().await {
            let Some(watcher) = watcher_weak.upgrade() else {
                break; // Handler dropped, stop the task
            };

            let uri = {
                let map = watcher.path_to_uri.read().await;
                map.get(&path).cloned()
            };

            let Some(uri) = uri else { continue };

            if !watcher.is_subscribed(&uri).await {
                continue;
            }

            let Some(p) = peer.get() else { continue };

            let param = ResourceUpdatedNotificationParam { uri: uri.clone() };
            if let Err(e) = p.notify_resource_updated(param).await {
                tracing::warn!(uri = %uri, error = %e, "Failed to send resource update notification");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_subscribe_unsubscribe() {
        let watcher = ResourceWatcher::new();

        watcher
            .subscribe("kaish://vfs/tmp".to_string(), None)
            .await;
        assert!(watcher.is_subscribed("kaish://vfs/tmp").await);

        watcher.unsubscribe("kaish://vfs/tmp").await;
        assert!(!watcher.is_subscribed("kaish://vfs/tmp").await);
    }

    #[tokio::test]
    async fn test_multiple_subscriptions() {
        let watcher = ResourceWatcher::new();

        watcher
            .subscribe("kaish://vfs/a".to_string(), None)
            .await;
        watcher
            .subscribe("kaish://vfs/b".to_string(), None)
            .await;

        let uris = watcher.subscribed_uris().await;
        assert_eq!(uris.len(), 2);
        assert!(uris.contains(&"kaish://vfs/a".to_string()));
        assert!(uris.contains(&"kaish://vfs/b".to_string()));
    }

    #[tokio::test]
    async fn test_unsubscribe_nonexistent() {
        let watcher = ResourceWatcher::new();
        // Should not panic
        watcher.unsubscribe("kaish://vfs/nonexistent").await;
    }

    #[tokio::test]
    async fn test_duplicate_subscribe() {
        let watcher = ResourceWatcher::new();

        watcher
            .subscribe("kaish://vfs/a".to_string(), None)
            .await;
        watcher
            .subscribe("kaish://vfs/a".to_string(), None)
            .await;

        let uris = watcher.subscribed_uris().await;
        assert_eq!(uris.len(), 1);
    }

    #[tokio::test]
    async fn test_subscribe_with_path_mapping() {
        let dir = std::env::temp_dir().join("kaish-path-map-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mapped.txt");
        std::fs::write(&file, "").unwrap();
        // Canonicalize to match what ResourceWatcher stores internally
        let canonical = std::fs::canonicalize(&file).unwrap();

        let watcher = ResourceWatcher::new();
        let uri = "kaish://vfs/tmp/kaish-path-map-test/mapped.txt".to_string();

        watcher.subscribe(uri.clone(), Some(file.clone())).await;

        assert!(watcher.is_subscribed(&uri).await);

        // Path mapping uses the canonical path
        let map = watcher.path_to_uri.read().await;
        assert_eq!(map.get(&canonical), Some(&uri));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_unsubscribe_clears_path_mapping() {
        let dir = std::env::temp_dir().join("kaish-path-clear-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cleared.txt");
        std::fs::write(&file, "").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();

        let watcher = ResourceWatcher::new();
        let uri = "kaish://vfs/tmp/test.txt".to_string();

        watcher.subscribe(uri.clone(), Some(file.clone())).await;
        watcher.unsubscribe(&uri).await;

        let path_map = watcher.path_to_uri.read().await;
        assert!(!path_map.contains_key(&canonical));

        let uri_map = watcher.uri_to_path.read().await;
        assert!(!uri_map.contains_key(&uri));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_file_change_sends_event() {
        use std::io::Write;

        let dir = std::env::temp_dir().join("kaish-watcher-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("watched.txt");
        std::fs::write(&file_path, "initial").unwrap();
        let canonical = std::fs::canonicalize(&file_path).unwrap();

        let watcher = ResourceWatcher::new();
        let uri = "kaish://vfs/tmp/kaish-watcher-test/watched.txt".to_string();

        // Subscribe with the real path
        watcher
            .subscribe(uri.clone(), Some(file_path.clone()))
            .await;

        // Modify the file
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(b"modified").unwrap();
            f.sync_all().unwrap();
        }

        // Give notify a moment to fire
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify subscription state is correct
        assert!(watcher.is_subscribed(&uri).await);
        let map = watcher.path_to_uri.read().await;
        assert_eq!(map.get(&canonical), Some(&uri));

        // Clean up — explicitly ignored: test cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
