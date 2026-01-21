//! Test utilities for kaish.
//!
//! Provides parsers and runners for the test file formats used in the kaish project:
//! - `tests/lexer/tokens.txt` — line-separated lexer tests
//! - `tests/parser/*.test` — markdown-like parser tests with expected AST
//! - `tests/eval/*.test` — evaluation tests with expected stdout/stderr/exit

pub mod lexer;
pub mod parser;
pub mod sexpr;

use std::fmt;

/// The result of running a single test case.
#[derive(Debug, Clone)]
pub enum TestResult {
    /// Test passed.
    Pass,
    /// Test failed with expected vs actual mismatch.
    Fail { expected: String, actual: String },
    /// Test was skipped.
    Skip { reason: String },
    /// Error running the test.
    Error { message: String },
}

impl TestResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, TestResult::Pass)
    }

    pub fn is_fail(&self) -> bool {
        matches!(self, TestResult::Fail { .. })
    }
}

/// Summary of running multiple test cases.
#[derive(Debug, Default)]
pub struct TestSummary {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub failures: Vec<TestFailure>,
}

/// A single test failure with context.
#[derive(Debug, Clone)]
pub struct TestFailure {
    pub name: String,
    pub line: usize,
    pub result: TestResult,
}

impl TestSummary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, name: impl Into<String>, line: usize, result: TestResult) {
        match &result {
            TestResult::Pass => self.passed += 1,
            TestResult::Fail { .. } => {
                self.failed += 1;
                self.failures.push(TestFailure {
                    name: name.into(),
                    line,
                    result,
                });
            }
            TestResult::Skip { .. } => {
                self.skipped += 1;
            }
            TestResult::Error { .. } => {
                self.errors += 1;
                self.failures.push(TestFailure {
                    name: name.into(),
                    line,
                    result,
                });
            }
        }
    }

    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped + self.errors
    }

    pub fn all_passed(&self) -> bool {
        self.failed == 0 && self.errors == 0
    }
}

impl fmt::Display for TestSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\n{}", "═".repeat(60))?;
        writeln!(f, "Test Summary: {} total", self.total())?;
        writeln!(
            f,
            "  ✓ {} passed  ✗ {} failed  ⊘ {} skipped  ⚠ {} errors",
            self.passed, self.failed, self.skipped, self.errors
        )?;

        if !self.failures.is_empty() {
            writeln!(f, "\nFailures:")?;
            for failure in &self.failures {
                writeln!(f, "\n  {} (line {})", failure.name, failure.line)?;
                match &failure.result {
                    TestResult::Fail { expected, actual } => {
                        writeln!(f, "    expected: {}", expected)?;
                        writeln!(f, "    actual:   {}", actual)?;
                    }
                    TestResult::Error { message } => {
                        writeln!(f, "    error: {}", message)?;
                    }
                    _ => {}
                }
            }
        }
        writeln!(f, "{}", "═".repeat(60))?;
        Ok(())
    }
}
