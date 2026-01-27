//! Integration tests for pre-execution validation.
//!
//! These tests verify that the validator correctly blocks execution
//! for scripts with Error-level issues, while allowing scripts with
//! only Warning-level issues to execute.

use kaish_kernel::Kernel;

/// Helper to create a transient kernel for testing.
async fn make_kernel() -> Kernel {
    Kernel::transient().expect("should create kernel")
}

// ============================================================================
// Tests that verify validation BLOCKS execution (Error-level issues)
// ============================================================================

#[tokio::test]
async fn validation_blocks_break_outside_loop() {
    let kernel = make_kernel().await;
    let result = kernel.execute("break").await;

    assert!(result.is_err(), "break outside loop should fail validation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("loop") || err.contains("validation"),
        "error should mention loop or validation: {}",
        err
    );
}

#[tokio::test]
async fn validation_blocks_continue_outside_loop() {
    let kernel = make_kernel().await;
    let result = kernel.execute("continue").await;

    assert!(result.is_err(), "continue outside loop should fail validation");
}

#[tokio::test]
async fn validation_blocks_return_outside_function() {
    let kernel = make_kernel().await;
    let result = kernel.execute("return").await;

    assert!(result.is_err(), "return outside function should fail validation");
}

#[tokio::test]
async fn validation_blocks_invalid_regex() {
    let kernel = make_kernel().await;
    // Unclosed bracket is invalid regex
    let result = kernel.execute("grep '[' /dev/null").await;

    assert!(result.is_err(), "invalid regex should fail validation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("regex") || err.contains("validation"),
        "error should mention regex or validation: {}",
        err
    );
}

#[tokio::test]
async fn validation_blocks_seq_zero_increment() {
    let kernel = make_kernel().await;
    // seq FIRST INCREMENT LAST with increment=0 would loop forever
    let result = kernel.execute("seq 1 0 10").await;

    assert!(result.is_err(), "seq with zero increment should fail validation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("zero") || err.contains("increment") || err.contains("validation"),
        "error should mention zero/increment or validation: {}",
        err
    );
}

// ============================================================================
// Tests that verify validation ALLOWS execution (Warning-level issues only)
// ============================================================================

#[tokio::test]
async fn validation_allows_unknown_command_with_warning() {
    let kernel = make_kernel().await;
    // Unknown command is a warning, not error
    // It will fail at runtime, not validation
    let result = kernel.execute("nonexistent_command_xyz").await;

    // Should get past validation but may fail at runtime
    // The key test is that it doesn't fail with "validation failed"
    match result {
        Ok(exec_result) => {
            // Got past validation, runtime failure is OK
            assert!(!exec_result.ok(), "unknown command should fail at runtime");
        }
        Err(e) => {
            let err = e.to_string();
            // Should NOT fail due to validation
            assert!(
                !err.contains("validation failed"),
                "unknown command should be warning not error: {}",
                err
            );
        }
    }
}

#[tokio::test]
async fn validation_allows_undefined_variable_with_warning() {
    let kernel = make_kernel().await;
    // Undefined variable is a warning
    let result = kernel.execute("echo $UNDEFINED_VARIABLE_XYZ").await;

    // Should succeed (variables expand to empty string)
    match result {
        Ok(exec_result) => {
            assert!(exec_result.ok(), "undefined var should expand to empty");
        }
        Err(e) => {
            let err = e.to_string();
            assert!(
                !err.contains("validation failed"),
                "undefined variable should be warning: {}",
                err
            );
        }
    }
}

// ============================================================================
// Tests for skip_validation flag
// ============================================================================

#[tokio::test]
async fn skip_validation_allows_break_outside_loop() {
    use kaish_kernel::KernelConfig;

    let config = KernelConfig::transient().with_skip_validation(true);
    let kernel = Kernel::new(config).expect("should create kernel");

    // With validation skipped, break outside loop passes validation
    // Runtime behavior: break at top level may be ignored or cause an error
    let result = kernel.execute("break").await;

    match result {
        Ok(_) => {
            // Got past validation - this is the key assertion
            // Runtime may succeed (break ignored) or fail, either is acceptable
        }
        Err(e) => {
            let err = e.to_string();
            // Should NOT say "validation failed" since we skipped it
            assert!(
                !err.contains("validation failed"),
                "should not fail validation when skipped: {}",
                err
            );
        }
    }
}

// ============================================================================
// Tests that valid scripts pass validation
// ============================================================================

#[tokio::test]
async fn validation_passes_for_valid_script() {
    let kernel = make_kernel().await;

    // A completely valid script
    let result = kernel.execute(r#"
        x=1
        echo $x
    "#).await;

    assert!(result.is_ok(), "valid script should pass validation");
    let exec = result.unwrap();
    assert!(exec.ok(), "valid script should execute successfully");
}

#[tokio::test]
async fn validation_passes_for_loop_with_break() {
    let kernel = make_kernel().await;

    // break inside a loop is valid
    let result = kernel.execute(r#"
        for i in 1 2 3; do
            if [[ $i == 2 ]]; then
                break
            fi
            echo $i
        done
    "#).await;

    assert!(result.is_ok(), "break inside loop should pass validation");
    let exec = result.unwrap();
    assert!(exec.ok(), "loop with break should execute successfully");
}

#[tokio::test]
async fn validation_passes_for_valid_grep() {
    let kernel = make_kernel().await;

    // Valid regex pattern
    let result = kernel.execute("echo 'hello world' | grep 'hello'").await;

    assert!(result.is_ok(), "valid grep should pass validation");
    let exec = result.unwrap();
    assert!(exec.ok(), "valid grep should execute successfully");
}

#[tokio::test]
async fn validation_passes_for_valid_seq() {
    let kernel = make_kernel().await;

    // Non-zero increment is valid
    let result = kernel.execute("seq 1 2 10").await;

    assert!(result.is_ok(), "valid seq should pass validation");
    let exec = result.unwrap();
    assert!(exec.ok(), "valid seq should execute successfully");
    assert!(exec.out.contains("1") && exec.out.contains("9"));
}
