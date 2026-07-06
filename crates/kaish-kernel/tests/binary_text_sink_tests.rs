//! Kernel-routed regression tests for binary (`Value::Bytes`) at TEXT SINKS.
//!
//! A binary value captured via `$(...)` (e.g. `b=$(cat blob)` — `cat` of a
//! non-UTF-8 file yields `Value::Bytes`) must go LOUD when it reaches a text
//! sink — string interpolation (`"x=$b"`), a bare word into an external-command
//! argv (`prog $b`), or the `echo` text-output builtin — never render the
//! `[binary: N bytes]` placeholder. The placeholder where the user's real bytes
//! should be is silent data corruption (crash beats corrupt).
//!
//! All tests route through `kernel.execute()` so the full dispatch chain runs.

// Test-fixture code: unwrap/expect on known-good setup is the idiom.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "localfs")]

mod common;

use common::kernel_at;
use std::fs;
use tempfile::tempdir;

/// Invalid-UTF-8 octets so `cat` marks the capture binary (`Value::Bytes`)
/// rather than a `String` — that's the path that used to reach the placeholder.
const BIN: &[u8] = b"\xff\x00\xfe\x80\x01\xc0kaish\xf5";

/// Assert a script ran loud: either `execute` returned `Err`, or it returned a
/// nonzero `ExecResult` whose error names the binary problem — and in NO case
/// did the `[binary: N bytes]` placeholder leak into stdout OR stderr.
///
/// Checks for `"cannot be used as"` rather than a bare `"binary"` substring —
/// every text-sink guard in this PR (`value_to_text_sink[_named]`) shares that
/// exact wording, whereas a merely-coincidental "binary" (e.g. the leaked
/// placeholder itself embedded in an unrelated "No such file or directory"
/// message) would false-pass a bare substring check.
async fn assert_loud_binary(script: &str) {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("src.bin"), BIN).unwrap();
    let kernel = kernel_at(dir.path());
    match kernel.execute(script).await {
        Ok(r) => {
            assert_ne!(r.code, 0, "binary at a text sink must be a nonzero exit: {script:?}");
            assert!(
                r.err.contains("cannot be used as"),
                "error should name the binary problem, got err={:?} for {script:?}",
                r.err
            );
            assert!(
                !r.text_out().contains("[binary") && !r.err.contains("[binary"),
                "the placeholder must NOT leak to stdout or stderr: out={:?} err={:?}",
                r.text_out(),
                r.err
            );
        }
        Err(e) => {
            // The alternate `{:#}` form walks the full `anyhow` cause chain —
            // some paths (e.g. `Stmt::Assignment`) wrap the real cause behind
            // a generic `.context("failed to evaluate assignment")`, so a
            // bare `{}` would only show that wrapper and miss the actual
            // binary-data message underneath.
            let msg = format!("{:#}", e);
            assert!(
                msg.contains("cannot be used as"),
                "execute error should name the binary problem, got {msg:?} for {script:?}"
            );
        }
    }
}

#[tokio::test]
async fn interpolating_a_binary_capture_is_loud_not_placeholder() {
    // The primary live bug: capture bytes, then splice into a string.
    assert_loud_binary(r#"b=$(cat src.bin); echo "x=$b""#).await;
}

#[tokio::test]
async fn bare_word_binary_into_echo_is_loud() {
    // `echo` is a pure text-output sink; a bare `$b` binary word goes loud.
    assert_loud_binary("b=$(cat src.bin); echo $b").await;
}

#[tokio::test]
async fn binary_arg_into_printf_is_loud() {
    // `printf` is a pure text-output sink like `echo`; a binary operand goes
    // loud instead of the `[binary: N bytes]` placeholder (kaibo C1).
    assert_loud_binary(r#"b=$(cat src.bin); printf "val=%s\n" "$b""#).await;
}

#[tokio::test]
async fn binary_in_default_expansion_is_loud() {
    // `${b:-fallback}` where b is present-and-binary must also go loud, not
    // render the placeholder (the value is present, so the default never fires).
    assert_loud_binary(r#"b=$(cat src.bin); echo "v=${b:-none}""#).await;
}

#[tokio::test]
async fn text_capture_interpolation_is_unaffected() {
    // Control: a normal text var still interpolates fine.
    let dir = tempdir().unwrap();
    let kernel = kernel_at(dir.path());
    let result = kernel.execute(r#"t=$(echo hi); echo "x=$t""#).await.unwrap();
    assert_eq!(result.code, 0, "text var must still work: {}", result.err);
    assert_eq!(result.text_out().trim(), "x=hi");
}

#[tokio::test]
async fn text_capture_bare_word_is_unaffected() {
    let dir = tempdir().unwrap();
    let kernel = kernel_at(dir.path());
    let result = kernel.execute("t=$(echo hi); echo $t").await.unwrap();
    assert_eq!(result.code, 0, "text var must still work: {}", result.err);
    assert_eq!(result.text_out().trim(), "hi");
}

/// External-command argv (`build_args_flat`): a bare `$b` binary word must go
/// loud crossing the process boundary. Gated on Linux + subprocess because it
/// spawns a real `/bin/echo`.
#[cfg(all(target_os = "linux", feature = "subprocess"))]
#[tokio::test]
async fn bare_word_binary_into_external_argv_is_loud() {
    assert_loud_binary("b=$(cat src.bin); /bin/echo $b").await;
}

// ── Remaining text sinks (GH #93 item 1) ──
//
// The primary sinks above (string interpolation, bare-word external argv,
// `echo`) were fixed in 0.11.0 via `value_to_text_sink`. These tests cover
// the sinks that still fell through to `value_to_string`'s `[binary: N
// bytes]` placeholder: builtin path-positional coercion, env-var export, the
// redirect target, and the `==`/`in`/`case`-glob semantic ops.

/// `mkdir`'s path-positional loop used to `value_to_string` a `Value::Bytes`
/// operand straight into a directory name.
#[tokio::test]
async fn mkdir_binary_path_positional_is_loud() {
    assert_loud_binary("b=$(cat src.bin); mkdir $b").await;
}

/// `rm`'s path-positional loop, same class as `mkdir`.
#[tokio::test]
async fn rm_binary_path_positional_is_loud() {
    assert_loud_binary("b=$(cat src.bin); rm $b").await;
}

/// `cp`'s destination operand — also exercises the restructured single-source
/// gate-overwrite path that used to stringify the source a second time.
#[tokio::test]
async fn cp_binary_dest_is_loud() {
    assert_loud_binary("b=$(cat src.bin); cp src.bin $b").await;
}

/// `ls`'s multi-path positional list (the `.map(value_to_string).collect()`
/// pattern shared by `find`/`grep`/`sed -i`).
#[tokio::test]
async fn ls_binary_path_positional_is_loud() {
    assert_loud_binary("b=$(cat src.bin); ls $b").await;
}

/// `[[ -f $b ]]` — the VFS-aware `FileTest` arm in `kernel.rs::eval_test_async`
/// used to stat a file literally named `[binary: N bytes]`.
#[tokio::test]
async fn double_bracket_file_test_binary_path_is_loud() {
    assert_loud_binary("b=$(cat src.bin); if [[ -f $b ]]; then echo hit; fi").await;
}

/// `test -f $b` — the `test` builtin's own (separate) file-test implementation,
/// which mirrors `[[`'s but must independently guard the same way.
#[tokio::test]
async fn test_builtin_file_test_binary_path_is_loud() {
    assert_loud_binary("b=$(cat src.bin); test -f $b").await;
}

/// A bare `$b` binary word as a redirect target used to become a file
/// literally named `[binary: N bytes]` instead of erroring — the collection
/// guard (`structured_boundary_error`) already fired for lists/records, but
/// binary fell through `eval_redirect_target`'s local `value_to_string`.
#[tokio::test]
async fn redirect_target_binary_is_loud() {
    assert_loud_binary("b=$(cat src.bin); echo hi > $b").await;
}

/// `case $b in ...)` glob-matched against the `[binary: N bytes]` placeholder
/// instead of erroring — Decision E territory, same as `==`/`in`.
#[tokio::test]
async fn case_glob_binary_operand_is_loud() {
    assert_loud_binary(
        r#"b=$(cat src.bin); case $b in
    x) echo matched ;;
    *) echo default ;;
esac"#,
    )
    .await;
}

/// `[[ $b == x ]]` used to fall through `values_equal`'s mixed-scalar arm and
/// silently compare the *other* side's text against the `[binary: N bytes]`
/// placeholder rather than erroring.
#[tokio::test]
async fn equality_binary_operand_is_loud() {
    assert_loud_binary(r#"b=$(cat src.bin); if [[ $b == x ]]; then echo hit; fi"#).await;
}

/// `[[ $b in $record ]]` — record-key membership used to stringify the
/// binary needle into a lookup key via `value_to_string`.
#[tokio::test]
async fn membership_binary_needle_against_record_is_loud() {
    assert_loud_binary(r#"b=$(cat src.bin); r={"k": 1}; if [[ $b in $r ]]; then echo hit; fi"#)
        .await;
}

/// Exporting a binary value and then running ANY external command used to
/// silently pass `[binary: N bytes]` as the child's env var value. Gated like
/// the argv test above — spawns a real process.
#[cfg(all(target_os = "linux", feature = "subprocess"))]
#[tokio::test]
async fn env_export_of_binary_value_is_loud() {
    assert_loud_binary("b=$(cat src.bin); export BIN=$b; /bin/true").await;
}

/// A binary value spliced into a heredoc body. Heredocs in real scripts
/// resolve through the async evaluator (`kernel.rs`), which already composes
/// through the guarded `eval_string_part_async` — this locks in that the
/// user-visible behavior is loud (the *sync* evaluator's own heredoc
/// assembly, `interpreter/eval.rs`, is covered directly in eval.rs's unit
/// tests since it isn't reachable from a real script).
#[tokio::test]
async fn heredoc_body_binary_var_is_loud() {
    assert_loud_binary("b=$(cat src.bin); cat <<EOF\n$b\nEOF").await;
}

/// A binary value used as a record's interpolated key (`{"$b": 1}`). Same
/// async-composition note as the heredoc test above.
#[tokio::test]
async fn record_key_binary_var_is_loud() {
    assert_loud_binary(r#"b=$(cat src.bin); r={"$b": 1}"#).await;
}

/// Control: `${#b}` is the byte count, not a loud error — binary length is
/// well-defined (unlike splicing it into text), so this must NOT regress into
/// erroring. Locks in the existing `value_length` behavior this PR relies on.
#[tokio::test]
async fn length_of_binary_is_byte_count_not_loud() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("src.bin"), BIN).unwrap();
    let kernel = kernel_at(dir.path());
    let result = kernel.execute("b=$(cat src.bin); echo ${#b}").await.unwrap();
    assert_eq!(result.code, 0, "err={}", result.err);
    assert_eq!(result.text_out().trim(), BIN.len().to_string());
}
