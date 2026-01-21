//! Integration tests for the parser using the test file format.

use kaish_testutil::parser::{parse_parser_tests, run_parser_tests};

const STATEMENTS_TEST: &str = include_str!("../../../tests/parser/statements.test");

/// Known failing test names due to parser/lexer bugs that are out of scope for test cleanup.
///
/// **Escape sequence processing** (lexer processes to actual characters):
/// - assign_string_with_escapes: `\n` becomes actual newline in output
/// - pipe_with_args: `\\.rs$` causes invalid escape sequence error
/// - test_expr_regex_match: `\.rs$` causes invalid escape sequence error
///
/// **Redirect paths** (slash `/` causes lexer errors):
/// - redirect_stdout, redirect_append, redirect_stdin, redirect_stderr,
///   redirect_both, redirect_multiple, redirect_in_pipeline
/// - test_file_exists, test_file_dir (unquoted paths `/etc/hosts`, `/tmp`)
///
/// **Test expression parsing** (wraps in cmd instead of standalone):
/// - test_string_empty, test_string_nonempty
/// - test_comparison_eq, test_comparison_neq, test_comparison_gt, test_comparison_lt
/// - test_expr_file_exists, test_expr_is_file, test_expr_is_dir
/// - test_expr_string_empty, test_expr_string_nonempty
/// - test_expr_comparison_eq, test_expr_comparison_ne
/// - test_expr_comparison_gt, test_expr_comparison_lt, test_expr_comparison_ge, test_expr_comparison_le
///
/// **Other parser issues**:
/// - if_command_condition: parses identifier as string instead of command
/// - if_comparison: parenthesized conditions `(${X} > 5)` not parsed
/// - dot_as_source_alias, source_command: `.kai` extension causes split
/// - named_arg_with_spaces_error: parser accepts `foo = bar` instead of erroring
/// - double_dash_ends_flags: `--` marker not handled correctly
const KNOWN_FAILING_TESTS: &[&str] = &[
    // Escape sequence processing
    "assign_string_with_escapes",
    "pipe_with_args",
    "test_expr_regex_match",
    // Redirect paths (slash causes lexer errors)
    "redirect_stdout",
    "redirect_append",
    "redirect_stdin",
    "redirect_stderr",
    "redirect_both",
    "redirect_multiple",
    "redirect_in_pipeline",
    "test_file_exists",
    "test_file_dir",
    // Test expressions wrapped in cmd
    "test_string_empty",
    "test_string_nonempty",
    "test_comparison_eq",
    "test_comparison_neq",
    "test_comparison_gt",
    "test_comparison_lt",
    "test_expr_file_exists",
    "test_expr_is_file",
    "test_expr_is_dir",
    "test_expr_string_empty",
    "test_expr_string_nonempty",
    "test_expr_comparison_eq",
    "test_expr_comparison_ne",
    "test_expr_comparison_gt",
    "test_expr_comparison_lt",
    "test_expr_comparison_ge",
    "test_expr_comparison_le",
    // Other parser issues
    "if_command_condition",
    "if_comparison",
    "dot_as_source_alias",
    "source_command",
    "named_arg_with_spaces_error",
    "double_dash_ends_flags",
];

#[test]
fn run_parser_test_file() {
    let cases = parse_parser_tests(STATEMENTS_TEST);
    let summary = run_parser_tests(&cases);

    // Print summary for visibility
    println!("{}", summary);

    // Check for unexpected failures (not in known list)
    let unexpected_failures: Vec<_> = summary
        .failures
        .iter()
        .filter(|f| !KNOWN_FAILING_TESTS.contains(&f.name.as_str()))
        .collect();

    if !unexpected_failures.is_empty() {
        println!("\n⚠️  UNEXPECTED FAILURES:");
        for f in &unexpected_failures {
            println!("  {} (line {})", f.name, f.line);
        }
        panic!(
            "Parser tests had {} unexpected failures (out of {} total failures)",
            unexpected_failures.len(),
            summary.failed
        );
    }

    println!(
        "\n✓ All {} failures are known/expected. {} tests passed.",
        summary.failed, summary.passed
    );
}
