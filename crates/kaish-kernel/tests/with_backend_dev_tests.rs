//! `Kernel::with_backend` must expose `/dev` (DevFs) regardless of what the
//! embedder's own backend does — this is the kaijutsu bug: a read-only
//! embedder backend made `cmd > /dev/null` fail as a filesystem error
//! instead of silently discarding the write, because `/dev` was never
//! kernel-owned in the `with_backend` path.

// Test-fixture code: unwrap/expect on known-good setup is the idiom here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use kaish_kernel::vfs::{LocalFs, VfsRouter};
use kaish_kernel::{Kernel, KernelBackend, KernelConfig, LocalBackend};

/// Build a `with_backend` kernel whose entire embedder-owned root is
/// read-only — the same shape as kaijutsu's `LocalBackend::read_only("/")`.
fn read_only_backend_kernel(dir: &std::path::Path) -> Kernel {
    let mut vfs = VfsRouter::new();
    vfs.mount("/", LocalFs::read_only(dir));
    let backend: Arc<dyn KernelBackend> = Arc::new(LocalBackend::new(Arc::new(vfs)));
    Kernel::with_backend(backend, KernelConfig::isolated(), |_| {}, |_| {})
        .expect("with_backend kernel")
}

async fn run(kernel: &Kernel, script: &str) -> (String, i64) {
    let result = kernel.execute(script).await.expect("kernel execute");
    (result.text_out().trim().to_string(), result.code)
}

#[tokio::test]
async fn dev_null_write_succeeds_under_read_only_backend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kernel = read_only_backend_kernel(dir.path());

    // Writing to /dev/null must be discarded, not rejected — /dev is
    // kernel-owned and must not depend on the embedder's backend being
    // writable.
    let (out, code) = run(&kernel, "echo hello > /dev/null").await;
    assert_eq!(code, 0, "write to /dev/null must succeed under a read-only backend: out={out:?}");
}

#[tokio::test]
async fn dev_null_reads_empty_under_with_backend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kernel = read_only_backend_kernel(dir.path());

    let (out, code) = run(&kernel, "cat /dev/null").await;
    assert_eq!(code, 0);
    assert_eq!(out, "", "reading /dev/null must be empty");
}

#[tokio::test]
async fn dev_zero_readable_under_with_backend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kernel = read_only_backend_kernel(dir.path());

    let (out, code) = run(&kernel, "head -c 8 /dev/zero | wc -c").await;
    assert_eq!(code, 0, "reading /dev/zero must succeed: out={out:?}");
    assert_eq!(out, "8", "head -c 8 /dev/zero should yield exactly 8 bytes: out={out:?}");
}
