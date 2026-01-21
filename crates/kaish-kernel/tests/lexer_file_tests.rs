//! Integration tests for the lexer using the test file format.

use kaish_testutil::lexer::{parse_lexer_tests, run_lexer_tests};

const TOKENS_TXT: &str = include_str!("../../../tests/lexer/tokens.txt");

/// Known failing lines due to design differences between test file and lexer.
/// All previously known failures have been fixed:
/// - Float display: 0.0 now shows decimal point
/// - Escape sequences: control chars now escaped for display
/// - Ambiguity checks: TRUE/yes/no now rejected as ambiguous
/// - Number-identifier: 123abc now rejected
/// - Float edge cases: .5 and 5. now rejected
const KNOWN_FAILING_LINES: &[usize] = &[
    // All lexer tests now pass!
];

#[test]
fn run_lexer_test_file() {
    let cases = parse_lexer_tests(TOKENS_TXT);
    let summary = run_lexer_tests(&cases);

    // Print summary for visibility
    println!("{}", summary);

    // Check for unexpected failures (not in known list)
    let unexpected_failures: Vec<_> = summary
        .failures
        .iter()
        .filter(|f| !KNOWN_FAILING_LINES.contains(&f.line))
        .collect();

    if !unexpected_failures.is_empty() {
        println!("\n⚠️  UNEXPECTED FAILURES:");
        for f in &unexpected_failures {
            println!("  Line {}: {}", f.line, f.name);
        }
        panic!(
            "Lexer tests had {} unexpected failures (out of {} total failures)",
            unexpected_failures.len(),
            summary.failed
        );
    }

    println!(
        "\n✓ All {} failures are known/expected. {} tests passed.",
        summary.failed, summary.passed
    );
}
