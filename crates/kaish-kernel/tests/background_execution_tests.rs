//! TDD tests for background execution (`&` operator).
//!
//! These tests document expected behavior. Implementation in kernel.rs
//! will make them pass.
//!
//! The `&` operator should:
//! - Spawn the command as a background job
//! - Register it with JobManager and create /v/jobs/{id}/ VFS entries
//! - Return immediately with a job ID like "[1]"
//! - Capture stdout/stderr via BoundedStream
//! - Allow polling status via /v/jobs/{id}/status

use std::time::Duration;

use kaish_kernel::{Kernel, KernelConfig};

/// Create a test kernel with an isolated (no local filesystem) configuration.
async fn setup() -> Kernel {
    Kernel::new(KernelConfig::isolated()).expect("failed to create kernel")
}

/// Wait for a job to complete by polling status.
///
/// This avoids flaky sleeps by checking actual job state.
async fn wait_for_job(kernel: &Kernel, job_id: u64, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    let status_cmd = format!("cat /v/jobs/{}/status", job_id);

    loop {
        let result = kernel.execute(&status_cmd).await.expect("status check failed");
        let status = result.out.trim();

        if status.starts_with("done:") || status.starts_with("failed:") {
            return status.to_string();
        }

        if start.elapsed() > timeout {
            panic!("Job {} did not complete within {:?}", job_id, timeout);
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ============================================================================
// Basic Background Execution
// ============================================================================

#[tokio::test]
async fn test_background_job_returns_job_id() {
    let kernel = setup().await;
    let result = kernel.execute("echo hello &").await.unwrap();

    assert!(result.ok(), "background command should succeed, got: {}", result.err);
    // Should return job ID like "[1]"
    assert!(
        result.out.contains("[1]") || result.out.contains("1"),
        "expected job ID in output, got: {}",
        result.out
    );
}

#[tokio::test]
async fn test_background_job_creates_vfs_entry() {
    let kernel = setup().await;
    kernel.execute("echo hello &").await.unwrap();

    // Give job a moment to register
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Job should appear in /v/jobs
    let result = kernel.execute("ls /v/jobs").await.unwrap();
    assert!(result.ok(), "ls /v/jobs failed: {}", result.err);
    assert!(
        result.out.contains("1"),
        "expected job 1 in /v/jobs, got: {}",
        result.out
    );
}

#[tokio::test]
async fn test_background_job_captures_stdout() {
    let kernel = setup().await;
    kernel.execute("echo 'hello from background' &").await.unwrap();

    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;

    let result = kernel.execute("cat /v/jobs/1/stdout").await.unwrap();
    assert!(result.ok(), "cat stdout failed: {}", result.err);
    assert!(
        result.out.contains("hello from background"),
        "expected stdout content, got: {}",
        result.out
    );
}

#[tokio::test]
async fn test_background_job_status_transitions() {
    let kernel = setup().await;
    kernel.execute("sleep 0.2 &").await.unwrap();

    // Give job a moment to register
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Check status while running (immediate check)
    let result = kernel.execute("cat /v/jobs/1/status").await.unwrap();
    assert!(result.ok(), "status check failed: {}", result.err);
    assert_eq!(result.out.trim(), "running", "expected running status");

    // Wait for completion
    let status = wait_for_job(&kernel, 1, Duration::from_secs(1)).await;
    assert_eq!(status, "done:0", "expected done:0 status");
}

#[tokio::test]
async fn test_background_job_command_file() {
    let kernel = setup().await;
    kernel.execute("echo test123 &").await.unwrap();

    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;

    let result = kernel.execute("cat /v/jobs/1/command").await.unwrap();
    assert!(result.ok(), "cat command failed: {}", result.err);
    assert!(
        result.out.contains("echo") && result.out.contains("test123"),
        "expected command in output, got: {}",
        result.out
    );
}

// ============================================================================
// Multiple Jobs
// ============================================================================

#[tokio::test]
async fn test_multiple_background_jobs() {
    let kernel = setup().await;

    kernel.execute("echo job1 &").await.unwrap();
    kernel.execute("echo job2 &").await.unwrap();
    kernel.execute("echo job3 &").await.unwrap();

    // Wait for all jobs
    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;
    wait_for_job(&kernel, 2, Duration::from_secs(1)).await;
    wait_for_job(&kernel, 3, Duration::from_secs(1)).await;

    // All jobs should exist
    let result = kernel.execute("ls /v/jobs").await.unwrap();
    assert!(result.ok());
    assert!(result.out.contains("1"), "missing job 1");
    assert!(result.out.contains("2"), "missing job 2");
    assert!(result.out.contains("3"), "missing job 3");
}

#[tokio::test]
async fn test_each_job_has_correct_output() {
    let kernel = setup().await;

    kernel.execute("echo 'output-one' &").await.unwrap();
    kernel.execute("echo 'output-two' &").await.unwrap();

    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;
    wait_for_job(&kernel, 2, Duration::from_secs(1)).await;

    let r1 = kernel.execute("cat /v/jobs/1/stdout").await.unwrap();
    let r2 = kernel.execute("cat /v/jobs/2/stdout").await.unwrap();

    assert!(r1.out.contains("output-one"), "job 1 wrong output: {}", r1.out);
    assert!(r2.out.contains("output-two"), "job 2 wrong output: {}", r2.out);
}

// ============================================================================
// Context Inheritance
// ============================================================================

#[tokio::test]
async fn test_background_job_inherits_env() {
    let kernel = setup().await;

    // Set environment variable
    kernel.execute("export MY_VAR=test_value").await.unwrap();
    kernel.execute("echo $MY_VAR &").await.unwrap();

    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;

    let result = kernel.execute("cat /v/jobs/1/stdout").await.unwrap();
    assert!(
        result.out.contains("test_value"),
        "expected env var in output, got: {}",
        result.out
    );
}

#[tokio::test]
async fn test_background_job_inherits_cwd() {
    let kernel = setup().await;

    // Create a unique test directory in the in-memory VFS and cd into it
    let dir = format!("/tmp/test_cwd_{}", std::process::id());
    kernel.execute(&format!("mkdir -p {dir}")).await.unwrap();
    kernel.execute(&format!("cd {dir}")).await.unwrap();
    kernel.execute("pwd &").await.unwrap();

    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;

    let result = kernel.execute("cat /v/jobs/1/stdout").await.unwrap();
    assert!(
        result.out.contains(&dir),
        "expected cwd in output, got: {}",
        result.out
    );
}

// ============================================================================
// Error Handling & Exit Codes
// ============================================================================

#[tokio::test]
async fn test_failed_background_job_status() {
    let kernel = setup().await;

    // Command that will fail (false returns exit code 1)
    kernel.execute("false &").await.unwrap();

    let status = wait_for_job(&kernel, 1, Duration::from_secs(1)).await;
    assert!(
        status.starts_with("failed:") || status == "done:1",
        "expected failed status, got: {}",
        status
    );
}

// ============================================================================
// Pipelines in Background
// ============================================================================

#[tokio::test]
async fn test_pipeline_in_background() {
    let kernel = setup().await;

    kernel.execute("echo 'line1\nline2\nline3' | wc -l &").await.unwrap();

    wait_for_job(&kernel, 1, Duration::from_secs(1)).await;

    let result = kernel.execute("cat /v/jobs/1/stdout").await.unwrap();
    assert!(result.ok(), "cat failed: {}", result.err);
    // wc -l should output "3"
    assert!(
        result.out.trim() == "3" || result.out.contains("3"),
        "expected 3 lines, got: {}",
        result.out
    );
}

// ============================================================================
// Jobs Builtin Integration
// ============================================================================

#[tokio::test]
async fn test_jobs_builtin_shows_background_job() {
    let kernel = setup().await;

    kernel.execute("echo background-test &").await.unwrap();

    // Give job a moment to register
    tokio::time::sleep(Duration::from_millis(10)).await;

    let result = kernel.execute("jobs").await.unwrap();
    assert!(result.ok(), "jobs command failed: {}", result.err);
    assert!(
        result.out.contains("1") && result.out.contains("/v/jobs/1/"),
        "expected job info, got: {}",
        result.out
    );
}
