//! Parser test file parser and runner.
//!
//! Parses the `tests/parser/*.test` format and runs tests against the kaish parser.

use crate::{TestResult, TestSummary};

/// A single parser test case.
#[derive(Debug, Clone)]
pub struct ParserTestCase {
    /// Test name from the `# test: name` line.
    pub name: String,
    /// Line number where the test starts (1-indexed).
    pub line_number: usize,
    /// The input source code.
    pub input: String,
    /// What we expect from parsing.
    pub expected: ParserExpectation,
}

/// What we expect from parsing an input.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserExpectation {
    /// Expected AST as S-expression.
    Ok(String),
    /// Expected error message.
    Error(String),
}

/// Parse the *.test file format into test cases.
pub fn parse_parser_tests(content: &str) -> Vec<ParserTestCase> {
    let mut cases = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        // Look for test header: # test: name
        if line.starts_with("# test:") {
            let name = line.strip_prefix("# test:").unwrap_or("").trim().to_string();
            let start_line = i + 1;
            i += 1;

            // Look for expect line: # expect: ok | error
            let expect_ok = if i < lines.len() {
                let expect_line = lines[i].trim();
                if expect_line.starts_with("# expect:") {
                    let expect = expect_line.strip_prefix("# expect:").unwrap_or("").trim();
                    i += 1;
                    expect == "ok"
                } else {
                    true
                }
            } else {
                true
            };

            // Skip to first ---
            while i < lines.len() && lines[i].trim() != "---" {
                i += 1;
            }
            i += 1; // skip the ---

            // Collect input until next ---
            let mut input_lines = Vec::new();
            while i < lines.len() && lines[i].trim() != "---" {
                input_lines.push(lines[i]);
                i += 1;
            }
            i += 1; // skip the ---

            // Collect expected until ===
            let mut expected_lines = Vec::new();
            while i < lines.len() && lines[i].trim() != "===" {
                expected_lines.push(lines[i]);
                i += 1;
            }
            i += 1; // skip the ===

            let input = input_lines.join("\n");
            let expected_str = expected_lines.join("\n").trim().to_string();

            let expected = if expect_ok {
                ParserExpectation::Ok(expected_str)
            } else {
                ParserExpectation::Error(expected_str)
            };

            cases.push(ParserTestCase {
                name,
                line_number: start_line,
                input,
                expected,
            });
        } else {
            i += 1;
        }
    }

    cases
}

impl ParserTestCase {
    /// Run this test case and return the result.
    pub fn run(&self) -> TestResult {
        use crate::sexpr::format_program;
        use kaish_kernel::parser::parse;

        match parse(&self.input) {
            Ok(program) => {
                let actual = format_program(&program);
                match &self.expected {
                    ParserExpectation::Ok(expected) => {
                        if normalize_sexpr(&actual) == normalize_sexpr(expected) {
                            TestResult::Pass
                        } else {
                            TestResult::Fail {
                                expected: expected.clone(),
                                actual,
                            }
                        }
                    }
                    ParserExpectation::Error(expected_error) => {
                        TestResult::Fail {
                            expected: format!("error: {}", expected_error),
                            actual,
                        }
                    }
                }
            }
            Err(errors) => {
                let error_msg = errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                match &self.expected {
                    ParserExpectation::Error(_) => {
                        // We expected an error - this is a pass
                        TestResult::Pass
                    }
                    ParserExpectation::Ok(expected) => {
                        TestResult::Fail {
                            expected: expected.clone(),
                            actual: format!("error: {}", error_msg),
                        }
                    }
                }
            }
        }
    }
}

/// Normalize S-expression for comparison (collapse whitespace).
fn normalize_sexpr(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run all parser test cases and return a summary.
pub fn run_parser_tests(cases: &[ParserTestCase]) -> TestSummary {
    let mut summary = TestSummary::new();

    for case in cases {
        let result = case.run();
        summary.record(&case.name, case.line_number, result);
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_test() {
        let content = r#"
# test: assign_int
# expect: ok
---
set X = 5
---
(assign X (int 5))
===
"#;
        let cases = parse_parser_tests(content);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "assign_int");
        assert_eq!(cases[0].input.trim(), "set X = 5");
        assert_eq!(
            cases[0].expected,
            ParserExpectation::Ok("(assign X (int 5))".to_string())
        );
    }

    #[test]
    fn parse_error_test() {
        let content = r#"
# test: error_case
# expect: error
---
bad syntax
---
error at 1:5: unexpected token
===
"#;
        let cases = parse_parser_tests(content);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "error_case");
        assert!(matches!(cases[0].expected, ParserExpectation::Error(_)));
    }
}
