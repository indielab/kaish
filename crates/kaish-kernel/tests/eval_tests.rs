//! Evaluation tests for kaish interpreter.
//!
//! All tests are ignored until the interpreter is implemented.
//! These serve as documentation and a roadmap for expected runtime behavior.

use rstest::rstest;

/// Run an eval test with expected stdout, stderr, and exit code.
#[allow(dead_code)]
fn run_eval_test(_script: &str, _expected_stdout: &str, _expected_stderr: &str, _expected_exit: i32) {
    // TODO: Implement when interpreter is ready
    unimplemented!("interpreter not yet implemented");
}

// =============================================================================
// ECHO & BASIC OUTPUT
// =============================================================================

#[rstest]
#[case::echo_string(r#"echo "hello""#, "hello\n", "", 0)]
#[case::echo_multiple(r#"echo "hello" "world""#, "hello world\n", "", 0)]
#[case::echo_empty(r#"echo """#, "\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_echo(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// VARIABLES
// =============================================================================

#[rstest]
#[case::var_simple(r#"set X = "hello"; echo ${X}"#, "hello\n", "", 0)]
#[case::var_int("set N = 42; echo ${N}", "42\n", "", 0)]
#[case::var_reassign(r#"set X = "first"; set X = "second"; echo ${X}"#, "second\n", "", 0)]
#[case::var_interpolation(r#"set NAME = "world"; echo "hello ${NAME}!""#, "hello world!\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_variables(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

#[rstest]
#[case::var_undefined("echo ${NOPE}", "", "error: undefined variable 'NOPE'\n", 1)]
#[ignore = "interpreter not implemented"]
fn eval_variable_errors(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// EXIT CODE ($?)
// =============================================================================

#[rstest]
#[case::exit_code_is_integer(r#"echo "test"; echo $?"#, "test\n0\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_exit_code(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// CONDITIONALS
// =============================================================================

#[rstest]
#[case::if_true_branch("set X = true; if ${X}; then echo \"yes\"; fi", "yes\n", "", 0)]
#[case::if_false_branch("set X = false; if ${X}; then echo \"yes\"; fi", "", "", 0)]
#[case::if_else_true("set X = true; if ${X}; then echo \"yes\"; else echo \"no\"; fi", "yes\n", "", 0)]
#[case::if_else_false("set X = false; if ${X}; then echo \"yes\"; else echo \"no\"; fi", "no\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_conditionals(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// AND/OR CHAINS
// =============================================================================

#[rstest]
#[case::and_both_succeed(r#"true && echo "both""#, "both\n", "", 0)]
#[case::or_first_fails(r#"false || echo "fallback""#, "fallback\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_chains(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// PIPES
// =============================================================================

#[rstest]
#[case::pipe_simple(r#"echo "hello" | cat"#, "hello\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_pipes(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// TOOL DEFINITIONS
// =============================================================================

#[rstest]
#[case::tool_call_simple(r#"tool greet name:string { echo "hello ${name}"; }; greet name="world""#, "hello world\n", "", 0)]
#[case::tool_call_default(r#"tool greet name:string="stranger" { echo "hello ${name}"; }; greet"#, "hello stranger\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_tool_definitions(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

#[rstest]
#[case::tool_missing_required(r#"tool greet name:string { echo "hello ${name}"; }; greet"#, "", "error: missing required parameter 'name'\n", 1)]
#[ignore = "interpreter not implemented"]
fn eval_tool_errors(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}

// =============================================================================
// TEST EXPRESSIONS [[ ]]
// =============================================================================

#[rstest]
#[case::test_string_comparison(r#"X="value"; if [[ $X == "value" ]]; then echo "match"; fi"#, "match\n", "", 0)]
#[case::test_string_empty(r#"X=""; if [[ -z $X ]]; then echo "empty"; fi"#, "empty\n", "", 0)]
#[case::test_string_nonempty(r#"X="hello"; if [[ -n $X ]]; then echo "has value"; fi"#, "has value\n", "", 0)]
#[ignore = "interpreter not implemented"]
fn eval_test_expressions(#[case] script: &str, #[case] stdout: &str, #[case] stderr: &str, #[case] exit: i32) {
    run_eval_test(script, stdout, stderr, exit);
}
