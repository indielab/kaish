# kaish Testing Strategy

Target: **10:1 test-to-feature ratio** before shipping.

## Test Categories

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Test Pyramid                                  │
│                                                                      │
│                         ╱╲      Fuzz tests (crash resistance)       │
│                        ╱  ╲                                          │
│                       ╱────╲    Property tests (invariants)         │
│                      ╱      ╲                                        │
│                     ╱────────╲  Golden file tests (behavior)        │
│                    ╱          ╲                                      │
│                   ╱────────────╲ Unit tests (each function)         │
│                  ╱              ╲                                    │
│                 ╱────────────────╲ Lexer token tests                │
└─────────────────────────────────────────────────────────────────────┘
```

## 1. Lexer Tests (`tests/lexer/`)

**Goal**: Every token type has exhaustive tests.

### Format: `tokens.txt`
```
INPUT | EXPECTED_TOKENS
INPUT | ERROR: message
```

### Coverage Requirements
- [ ] Every token variant has ≥5 valid examples
- [ ] Every token variant has ≥3 invalid/edge cases
- [ ] Boundary conditions (empty, max length, unicode)
- [ ] All error messages tested

### Test Count Target: ~200 lexer tests

## 2. Parser Tests (`tests/parser/`)

**Goal**: Every grammar production has exhaustive tests.

### Format: `*.test` files
```
# test: name
# expect: ok | error
---
input
---
expected AST or error
===
```

### Coverage Requirements
For EACH grammar production:
- [ ] Minimal valid example
- [ ] Complex valid example
- [ ] All optional parts present/absent
- [ ] Boundary cases (empty arrays, deep nesting)
- [ ] Invalid variants with good error messages

### Productions to Test
| Production | Min Tests |
|------------|-----------|
| assignment | 20 |
| command | 30 |
| pipeline | 20 |
| redirect | 15 |
| if_stmt | 15 |
| for_stmt | 10 |
| tool_def | 20 |
| value (all types) | 40 |
| var_ref | 20 |
| named_arg | 15 |
| array | 15 |
| object | 15 |

### Test Count Target: ~250 parser tests

## 3. Evaluation Tests (`tests/eval/`)

**Goal**: Every runtime behavior has deterministic tests.

### Format: `*.test` files
```
# test: name
# expect: ok | error
---
script
---
stdout: expected
stderr: expected
exit: code
===
```

### Coverage Requirements
- [ ] All builtins tested individually
- [ ] All builtins tested in combination
- [ ] Variable scoping rules
- [ ] Result type (`$?`) behavior
- [ ] Control flow (if/for/&&/||)
- [ ] Pipes and redirects
- [ ] Tool definition and invocation
- [ ] Background jobs
- [ ] Scatter/gather

### Test Count Target: ~300 eval tests

## 4. Property-Based Tests (`tests/properties/`)

**Goal**: Verify invariants hold across random inputs.

```rust
// properties.rs

use proptest::prelude::*;

// P1: Lexer never panics
proptest! {
    #[test]
    fn lexer_never_panics(input in ".*") {
        let _ = lex(&input);
    }
}

// P2: Parser never panics
proptest! {
    #[test]
    fn parser_never_panics(input in ".*") {
        let _ = parse(&input);
    }
}

// P3: Valid AST round-trips through pretty printer
proptest! {
    #[test]
    fn ast_roundtrip(ast in arb_valid_ast()) {
        let printed = pretty_print(&ast);
        let reparsed = parse(&printed).unwrap();
        prop_assert_eq!(ast, reparsed);
    }
}

// P4: Lexer is deterministic
proptest! {
    #[test]
    fn lexer_deterministic(input in ".*") {
        let t1 = lex(&input);
        let t2 = lex(&input);
        prop_assert_eq!(t1, t2);
    }
}

// P5: Parser is deterministic
proptest! {
    #[test]
    fn parser_deterministic(input in ".*") {
        let a1 = parse(&input);
        let a2 = parse(&input);
        prop_assert_eq!(a1, a2);
    }
}

// P6: Combining valid constructs produces valid programs
proptest! {
    #[test]
    fn valid_combinations(
        stmts in prop::collection::vec(arb_valid_stmt(), 1..10)
    ) {
        let program = stmts.join("\n");
        prop_assert!(parse(&program).is_ok());
    }
}

// P7: Errors have source locations
proptest! {
    #[test]
    fn errors_have_locations(input in arb_invalid_input()) {
        if let Err(e) = parse(&input) {
            prop_assert!(e.span.is_some(), "error should have location");
        }
    }
}
```

### Test Count Target: ~20 property tests × 1000 iterations = 20,000 test cases

## 5. Fuzz Tests (`fuzz/`)

**Goal**: The parser never crashes on arbitrary input.

```rust
// fuzz/fuzz_targets/lexer.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = kaish::lex(s);
    }
});

// fuzz/fuzz_targets/parser.rs
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = kaish::parse(s);
    }
});

// fuzz/fuzz_targets/eval.rs
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let mut shell = kaish::Shell::new_sandboxed();
        let _ = shell.execute(s);
    }
});
```

Run with: `cargo +nightly fuzz run parser -- -max_len=4096`

### Target: Run fuzzer for 24h with no crashes

## 6. Error Message Tests (`tests/errors/`)

**Goal**: Every error path produces a helpful message.

```
# test: undefined_var
---
echo ${NOPE}
---
error: undefined variable 'NOPE' at line 1, column 6
  |
1 | echo ${NOPE}
  |      ^^^^^^^ not defined
  |
help: did you mean to use 'set NOPE = ...' first?
===

# test: ambiguous_bool
---
set X = YES
---
error: ambiguous value 'YES' at line 1, column 9
  |
1 | set X = YES
  |         ^^^ could be boolean or string
  |
help: use 'true' for boolean or '"YES"' for string
===
```

### Test Count Target: ~100 error tests

## 7. Integration Tests (`tests/integration/`)

**Goal**: Full scripts work end-to-end.

```bash
#!/usr/bin/env kaish
# tests/integration/code_search.kai
# Integration test: search code and count results

set PATTERN = "fn "
set PATH = "/test-fixtures/rust-sample"

ls path=${PATH} recursive=true \
    | grep pattern="\.rs$" \
    | scatter as=FILE limit=4 \
    | grep-file pattern=${PATTERN} file=${FILE} \
    | gather \
    > /scratch/results.json

set COUNT = $(cat /scratch/results.json | jq path="length")
assert val=${COUNT} op=">" expected=0 msg="should find functions"
```

### Test Count Target: ~50 integration scripts

## Test Infrastructure

### Test Harness (`tests/harness.rs`)

```rust
use std::fs;
use std::path::Path;

#[derive(Debug)]
struct TestCase {
    name: String,
    input: String,
    expected: Expected,
}

#[derive(Debug)]
enum Expected {
    Ast(String),
    Output { stdout: String, stderr: String, exit: i32 },
    Error(String),
}

fn parse_test_file(path: &Path) -> Vec<TestCase> {
    let content = fs::read_to_string(path).unwrap();
    // Parse the test file format...
}

fn run_parser_test(test: &TestCase) -> Result<(), String> {
    let result = kaish::parse(&test.input);
    match (&result, &test.expected) {
        (Ok(ast), Expected::Ast(expected)) => {
            let actual = ast.to_sexp();
            if actual == *expected {
                Ok(())
            } else {
                Err(format!("AST mismatch:\nexpected: {}\nactual: {}", expected, actual))
            }
        }
        (Err(e), Expected::Error(expected)) => {
            if e.message.contains(expected) {
                Ok(())
            } else {
                Err(format!("Error mismatch:\nexpected: {}\nactual: {}", expected, e.message))
            }
        }
        _ => Err(format!("Unexpected result type"))
    }
}

#[test]
fn run_all_parser_tests() {
    let test_dir = Path::new("tests/parser");
    let mut failures = Vec::new();

    for entry in fs::read_dir(test_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|e| e == "test").unwrap_or(false) {
            for test in parse_test_file(&path) {
                if let Err(msg) = run_parser_test(&test) {
                    failures.push((test.name.clone(), msg));
                }
            }
        }
    }

    if !failures.is_empty() {
        for (name, msg) in &failures {
            eprintln!("FAIL: {}\n{}\n", name, msg);
        }
        panic!("{} tests failed", failures.len());
    }
}
```

### AST Generators for Property Tests

```rust
use proptest::prelude::*;

fn arb_ident() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,20}".prop_filter("not keyword", |s| {
        !["set", "tool", "if", "then", "else", "fi", "for", "in", "do", "done"]
            .contains(&s.as_str())
    })
}

fn arb_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(Value::Int),
        any::<f64>().prop_filter_map("finite", |f| {
            if f.is_finite() { Some(Value::Float(f)) } else { None }
        }),
        any::<bool>().prop_map(Value::Bool),
        "\"[^\"\\\\]{0,50}\"".prop_map(Value::String),
    ]
}

fn arb_valid_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        // Assignment
        (arb_ident(), arb_value()).prop_map(|(name, val)| {
            format!("set {} = {}", name, val.to_kaish())
        }),
        // Simple command
        (arb_ident(), prop::collection::vec(arb_value(), 0..3))
            .prop_map(|(cmd, args)| {
                let args_str = args.iter()
                    .map(|a| a.to_kaish())
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{} {}", cmd, args_str)
            }),
        // Echo
        arb_value().prop_map(|v| format!("echo {}", v.to_kaish())),
    ]
}
```

## Coverage Tracking

Use `cargo-tarpaulin` or `llvm-cov`:

```bash
cargo tarpaulin --out Html --output-dir coverage/
```

### Coverage Targets

| Module | Line Coverage | Branch Coverage |
|--------|--------------|-----------------|
| lexer | 95% | 90% |
| parser | 95% | 90% |
| interpreter | 90% | 85% |
| tools | 85% | 80% |
| vfs | 85% | 80% |

## Test Summary

| Category | Count | Purpose |
|----------|-------|---------|
| Lexer unit | ~200 | Token correctness |
| Parser unit | ~250 | Grammar correctness |
| Eval unit | ~300 | Behavior correctness |
| Properties | ~20 × 1000 | Invariant verification |
| Fuzz | continuous | Crash resistance |
| Error messages | ~100 | UX quality |
| Integration | ~50 | End-to-end |
| **Total** | **~21,000** | |

## CI Pipeline

```yaml
# .github/workflows/test.yml
name: Tests
on: [push, pull_request]

jobs:
  unit-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --all-features

  property-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --features proptest -- --ignored

  coverage:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-tarpaulin
      - run: cargo tarpaulin --out Xml
      - uses: codecov/codecov-action@v3

  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz
      - run: cargo +nightly fuzz run parser -- -max_total_time=300
```
