//! Shell variable and expression handling bug tests.
//!
//! These tests were written to catch bugs discovered during real-world testing.
//! Tests are written first (TDD style), then code is fixed to make them pass.

use kaish_kernel::Kernel;

// ============================================================================
// Bug 1: ${?} braced form returns 0 instead of actual exit code
// ============================================================================

#[tokio::test]
async fn test_braced_last_exit_code_after_success() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("true; echo ${?}").await.unwrap();
    assert_eq!(result.out.trim(), "0", "Expected 0 after true command");
}

#[tokio::test]
async fn test_braced_last_exit_code_after_failure() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("false; echo ${?}").await.unwrap();
    assert_eq!(result.out.trim(), "1", "Expected 1 after false command");
}

#[tokio::test]
async fn test_braced_vs_unbraced_exit_code_equivalence() {
    let kernel = Kernel::transient().unwrap();
    // Both forms should give the same result
    let result1 = kernel.execute("false; echo $?").await.unwrap();
    let result2 = kernel.execute("false; echo ${?}").await.unwrap();
    assert_eq!(
        result1.out.trim(),
        result2.out.trim(),
        "Braced and unbraced $? should be equivalent"
    );
}

// ============================================================================
// Bug 2: $? in arithmetic $(($? + 1)) fails
// ============================================================================

#[tokio::test]
async fn test_exit_code_in_arithmetic() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("false; echo $(( $? + 10 ))").await.unwrap();
    assert_eq!(result.out.trim(), "11", "Expected 1 + 10 = 11");
}

#[tokio::test]
async fn test_exit_code_in_arithmetic_after_success() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("true; echo $(( $? * 5 ))").await.unwrap();
    assert_eq!(result.out.trim(), "0", "Expected 0 * 5 = 0");
}

#[tokio::test]
async fn test_pid_in_arithmetic() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("echo $(( $$ % 100000 ))").await.unwrap();
    // Just verify it parses and returns a number
    let val: i64 = result.out.trim().parse().expect("Should be a number");
    assert!(val >= 0, "PID mod should be non-negative");
}

#[tokio::test]
async fn test_braced_exit_code_in_arithmetic() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("false; echo $(( ${?} + 5 ))").await.unwrap();
    assert_eq!(result.out.trim(), "6", "Expected 1 + 5 = 6");
}

// ============================================================================
// Bug 3: return N leaks value to stdout
// ============================================================================

#[tokio::test]
async fn test_return_does_not_output_value() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
f() {
    echo "output"
    return 5
}
X=$(f)
echo "captured: [$X]"
"#,
        )
        .await
        .unwrap();
    // The captured output should only contain "output", not the return value
    assert!(
        result.out.contains("captured: [output]"),
        "Expected 'captured: [output]', got: {}",
        result.out
    );
    // Should NOT contain JSON or the number 5 in the captured var
    assert!(
        !result.out.contains("captured: [5"),
        "Return value leaked to stdout: {}",
        result.out
    );
}

#[tokio::test]
async fn test_return_sets_exit_code() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
f() { return 42; }
f
echo $?
"#,
        )
        .await
        .unwrap();
    assert!(
        result.out.contains("42"),
        "Expected exit code 42, got: {}",
        result.out
    );
}

#[tokio::test]
async fn test_return_without_value() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
f() { echo "hi"; return; }
X=$(f)
echo "got: [$X]"
"#,
        )
        .await
        .unwrap();
    assert!(
        result.out.contains("got: [hi]"),
        "Expected 'got: [hi]', got: {}",
        result.out
    );
}

// ============================================================================
// Bug 4: local keyword doesn't scope variables
// ============================================================================

#[tokio::test]
async fn test_local_variable_scoping() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
x=outer
f() {
    local x = inner
    echo "in func: $x"
}
f
echo "after func: $x"
"#,
        )
        .await
        .unwrap();
    assert!(
        result.out.contains("in func: inner"),
        "Local var should be 'inner' inside function: {}",
        result.out
    );
    assert!(
        result.out.contains("after func: outer"),
        "Outer var should be 'outer' after function: {}",
        result.out
    );
}

#[tokio::test]
async fn test_local_does_not_affect_outer_scope() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
count=0
increment() {
    local count = 99
}
increment
echo $count
"#,
        )
        .await
        .unwrap();
    assert_eq!(
        result.out.trim(),
        "0",
        "Outer 'count' should still be 0: {}",
        result.out
    );
}

#[tokio::test]
async fn test_non_local_modifies_outer_scope() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
count=0
increment() {
    count=99
}
increment
echo $count
"#,
        )
        .await
        .unwrap();
    assert_eq!(
        result.out.trim(),
        "99",
        "Without local, 'count' should be modified: {}",
        result.out
    );
}

// ============================================================================
// Bug 5: Nested command substitution $(echo $(echo x)) fails
// ============================================================================

#[tokio::test]
async fn test_nested_command_substitution() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("echo $(echo $(echo hello))").await.unwrap();
    assert_eq!(
        result.out.trim(),
        "hello",
        "Nested cmd subst should work: {}",
        result.out
    );
}

#[tokio::test]
async fn test_deeply_nested_command_substitution() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute("echo $(echo $(echo $(echo deep)))")
        .await
        .unwrap();
    assert_eq!(
        result.out.trim(),
        "deep",
        "Deeply nested cmd subst should work: {}",
        result.out
    );
}

// ============================================================================
// Bug 6: Command substitution in for loops fails
// ============================================================================

#[tokio::test]
async fn test_command_subst_in_for_loop() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
for x in $(echo "a b c"); do
    echo "item: $x"
done
"#,
        )
        .await
        .unwrap();
    assert!(
        result.out.contains("item: a"),
        "Should have item a: {}",
        result.out
    );
    assert!(
        result.out.contains("item: b"),
        "Should have item b: {}",
        result.out
    );
    assert!(
        result.out.contains("item: c"),
        "Should have item c: {}",
        result.out
    );
}

#[tokio::test]
async fn test_command_subst_in_for_loop_with_pipeline() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
for num in $(echo "1 2 3"); do
    echo "number $num"
done
"#,
        )
        .await
        .unwrap();
    assert!(
        result.out.contains("number 1"),
        "Should have number 1: {}",
        result.out
    );
    assert!(
        result.out.contains("number 2"),
        "Should have number 2: {}",
        result.out
    );
    assert!(
        result.out.contains("number 3"),
        "Should have number 3: {}",
        result.out
    );
}

// ============================================================================
// Bug 7: >&2 and 1>&2 redirects don't parse
// ============================================================================

#[tokio::test]
async fn test_stdout_to_stderr_redirect_1_ampersand_2() {
    let kernel = Kernel::transient().unwrap();
    // This should parse and execute without error
    let result = kernel.execute("echo error 1>&2").await.unwrap();
    // Output should go to stderr, not stdout
    assert!(
        result.err.contains("error"),
        "Expected 'error' in stderr: stdout={}, stderr={}",
        result.out,
        result.err
    );
}

#[tokio::test]
async fn test_stdout_to_stderr_redirect_ampersand_2() {
    let kernel = Kernel::transient().unwrap();
    // Shorthand form: >&2 is equivalent to 1>&2
    let result = kernel.execute("echo warning >&2").await.unwrap();
    assert!(
        result.err.contains("warning"),
        "Expected 'warning' in stderr: stdout={}, stderr={}",
        result.out,
        result.err
    );
}

// ============================================================================
// Bug 8: ${$} braced PID form (same issue as ${?})
// ============================================================================

#[tokio::test]
async fn test_braced_current_pid() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel.execute("echo ${$}").await.unwrap();
    // Should be a positive integer (the PID)
    let pid: u32 = result
        .out
        .trim()
        .parse()
        .expect("${$} should be a number");
    assert!(pid > 0, "PID should be positive: {}", pid);
}

#[tokio::test]
async fn test_braced_vs_unbraced_pid_equivalence() {
    let kernel = Kernel::transient().unwrap();
    let result1 = kernel.execute("echo $$").await.unwrap();
    let result2 = kernel.execute("echo ${$}").await.unwrap();
    assert_eq!(
        result1.out.trim(),
        result2.out.trim(),
        "Braced and unbraced $$ should be equivalent"
    );
}

// ============================================================================
// Additional edge cases and combinations
// ============================================================================

#[tokio::test]
async fn test_exit_code_in_string_interpolation() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(r#"false; echo "exit code: $?""#)
        .await
        .unwrap();
    assert!(
        result.out.contains("exit code: 1"),
        "Expected 'exit code: 1': {}",
        result.out
    );
}

#[tokio::test]
async fn test_braced_exit_code_in_string_interpolation() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(r#"false; echo "exit code: ${?}""#)
        .await
        .unwrap();
    assert!(
        result.out.contains("exit code: 1"),
        "Expected 'exit code: 1': {}",
        result.out
    );
}

#[tokio::test]
async fn test_local_with_command_substitution() {
    let kernel = Kernel::transient().unwrap();
    let result = kernel
        .execute(
            r#"
val=original
f() {
    local val = $(echo "from_cmd")
    echo "local: $val"
}
f
echo "outer: $val"
"#,
        )
        .await
        .unwrap();
    assert!(
        result.out.contains("local: from_cmd"),
        "Local with cmd subst: {}",
        result.out
    );
    assert!(
        result.out.contains("outer: original"),
        "Outer unchanged: {}",
        result.out
    );
}
