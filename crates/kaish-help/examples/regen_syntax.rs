//! Regenerate `content/en/syntax.md` from the Syntax fragments.
//!
//! `content/en/syntax.md` is a committed, drift-tested mirror of
//! `kaish_help::render_syntax_reference()`. After editing any Syntax fragment in
//! `src/fragments.rs`, run:
//!
//! ```sh
//! cargo run -p kaish-help --example regen_syntax
//! ```
//!
//! The `syntax_md_matches_fragments` test fails until the file is regenerated.

fn main() -> std::io::Result<()> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/content/en/syntax.md");
    std::fs::write(path, kaish_help::render_syntax_reference())?;
    println!("regenerated {path}");
    Ok(())
}
