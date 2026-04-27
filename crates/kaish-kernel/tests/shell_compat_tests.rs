//! Prototype: compat tests that run the same script through kaish AND bash,
//! so we can spot where the two diverge.
//!
//! Each `shell_compat!` invocation generates a submodule with two tests:
//!   `<name>::kaish` — always runs, executes the script via the kaish kernel.
//!   `<name>::bash`  — runs only when `KAISH_BASH_COMPAT=1` is set, executes
//!                     the script via `bash -c` and applies the same
//!                     assertions to bash's stdout. Otherwise it returns
//!                     immediately as a no-op pass.
//!
//! Usage:
//!   cargo test --test shell_compat_tests                     # kaish only
//!   KAISH_BASH_COMPAT=1 cargo test --test shell_compat_tests # both sides
//!
//! `cargo test bash` filters to just the bash side and surfaces divergences.
//!
//! Caveats:
//! - This file is a *prototype*. The macro and helper would move to
//!   `tests/common/` before broader adoption (or to a small kaish-test crate
//!   if other test binaries want it).
//! - Tests with intended divergence from bash (no implicit word splitting,
//!   structured-data iteration, `local x = v` syntax, kaish-only builtins)
//!   are deliberately not included here. A future `bash_eq:` arm could
//!   document and test divergence explicitly.

use kaish_kernel::Kernel;
use std::process::Command;

/// Run a script via `bash -c` if KAISH_BASH_COMPAT is set; otherwise return
/// `None` and let the caller short-circuit. Panics if the user opted in but
/// `bash` is missing or errored — they explicitly asked us to compare.
fn run_bash_if_enabled(script: &str) -> Option<String> {
    if std::env::var_os("KAISH_BASH_COMPAT").is_none() {
        return None;
    }
    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .expect("KAISH_BASH_COMPAT set but failed to run bash; install bash or unset the var");
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// TT-muncher that turns the trailing `eq: / contains: / absent:` clauses of a
/// `shell_compat!` invocation into a sequence of asserts against `$out`.
macro_rules! shell_compat_assert {
    ($out:expr,) => {};
    ($out:expr, eq: $expected:expr $(, $($rest:tt)*)?) => {
        assert_eq!(
            $out.trim(),
            $expected,
            "stdout mismatch:\n--- got ---\n{}\n-----------",
            $out,
        );
        $( shell_compat_assert!($out, $($rest)*); )?
    };
    ($out:expr, contains: $needle:expr $(, $($rest:tt)*)?) => {
        assert!(
            $out.contains($needle),
            "missing substring {:?}:\n--- got ---\n{}\n-----------",
            $needle,
            $out,
        );
        $( shell_compat_assert!($out, $($rest)*); )?
    };
    ($out:expr, absent: $missing:expr $(, $($rest:tt)*)?) => {
        assert!(
            !$out.contains($missing),
            "unexpected substring {:?}:\n--- got ---\n{}\n-----------",
            $missing,
            $out,
        );
        $( shell_compat_assert!($out, $($rest)*); )?
    };
}

/// Generate a pair of tests for a single shell scenario.
///
/// The kaish side always runs. The bash side is gated on KAISH_BASH_COMPAT
/// so the default `cargo test` run is unaffected.
macro_rules! shell_compat {
    (
        name: $name:ident,
        script: $script:expr,
        $($body:tt)*
    ) => {
        mod $name {
            use super::*;

            fn check(out: &str) {
                shell_compat_assert!(out, $($body)*);
            }

            #[tokio::test]
            async fn kaish() {
                let kernel = Kernel::transient().expect("transient kernel");
                let result = kernel.execute($script).await.expect("kaish execute");
                let out: String = result.text_out().into_owned();
                check(&out);
            }

            #[test]
            fn bash() {
                let Some(out) = run_bash_if_enabled($script) else { return };
                check(&out);
            }
        }
    };
}

// ---- Example conversions from shell_bugs_tests.rs --------------------------

shell_compat! {
    name: braced_last_exit_code_after_success,
    script: "true; echo ${?}",
    eq: "0",
}

shell_compat! {
    name: braced_last_exit_code_after_failure,
    script: "false; echo ${?}",
    eq: "1",
}

shell_compat! {
    name: exit_code_in_arithmetic,
    script: "false; echo $(( $? + 10 ))",
    eq: "11",
}

shell_compat! {
    name: exit_code_in_arithmetic_after_success,
    script: "true; echo $(( $? * 5 ))",
    eq: "0",
}

shell_compat! {
    name: nested_command_substitution,
    script: "echo $(echo $(echo hello))",
    eq: "hello",
}

shell_compat! {
    name: deeply_nested_command_substitution,
    script: "echo $(echo $(echo $(echo deep)))",
    eq: "deep",
}

shell_compat! {
    name: return_does_not_leak_to_capture,
    script: "f() {\n    echo output\n    return 5\n}\nX=$(f)\necho \"captured: [$X]\"\n",
    contains: "captured: [output]",
    absent: "captured: [5",
}

shell_compat! {
    name: return_sets_exit_code,
    script: "f() { return 42; }\nf\necho $?\n",
    eq: "42",
}

// ---- Demonstrator: a known divergence -------------------------------------
//
// kaish does *not* split $(...) on whitespace; bash does. The kaish side
// passes (one iteration with the whole string); the bash side fails under
// KAISH_BASH_COMPAT=1, surfacing the divergence as a test failure rather
// than silent drift. A future `bash_eq:` arm could capture both expected
// outputs in one place and turn this into a passing-but-divergent record.

shell_compat! {
    name: command_subst_no_implicit_split,
    script: "for x in $(echo \"a b c\"); do echo \"[$x]\"; done",
    eq: "[a b c]",
}
