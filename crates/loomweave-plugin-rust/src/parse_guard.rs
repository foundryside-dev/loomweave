//! Pre-parse guards: syn 2.x has no recursion limit — deeply nested input
//! overflows the stack and SIGABRTs the process. Empirical first-crash depths
//! at 8 MiB stack (syn 2.0.117): nested mods 337, blocks 480, parens 760,
//! unary `!` 2386. Caps below sit ≥4x under the worst case at the 16 MiB
//! pinned parse stack. Basis/override/retune per ADR-035; retune on syn major
//! bump or [`PARSE_STACK_BYTES`] change. See ADR-050.
//!
//! The depth scan is a minimal byte-oriented lexer, not a parser: it must skip
//! comments (banner comments are full of `*`, a prefix-run byte; nested block
//! comments are legal Rust), string literals (incl. raw strings `r#"…"#`), and
//! char literals (`'('` must not count as a bracket) while distinguishing a
//! char literal from a lifetime tick (`'a`). Misclassification is
//! safe-by-construction: a false positive degrades the file to a visible
//! warning finding; a false negative is contained by the host's lifecycle
//! deadlines + abnormal-exit classification (ADR-050).

use std::path::Path;

/// Maximum bracket (`(`/`[`/`{`) nesting depth accepted before parsing.
/// Basis: lowest measured syn first-crash depth is 337 (nested mods, 8 MiB
/// stack); 128 sits ≥4x under every measured floor at the 16 MiB pinned stack.
/// Override: none (a source file this deep is hostile or generated).
/// Retune: re-run the depth probe on a syn major bump.
pub const MAX_BRACKET_DEPTH: usize = 128;

/// Maximum run of consecutive prefix-operator bytes (`!`, `&`, `*`, `-`).
/// Basis: unary bombs parse recursively but carry bracket depth ~1, so the
/// depth cap misses them; measured first-crash run is 2386 at 8 MiB (~4.7k at
/// 16 MiB). 1024 sits ≥4x under the 16 MiB floor. Override: none.
/// Retune: re-run the unary probe on a syn major bump.
pub const MAX_PREFIX_RUN: usize = 1024;

/// Maximum `.rs` file size read and parsed, in bytes. Basis: the manifest
/// declares `expected_max_rss_mb = 128`; a multi-MiB file's string + token
/// stream + AST can blow `RLIMIT_AS` and get the child killed — degrade
/// instead. Checked via `fs::metadata` BEFORE reading. Override: none.
/// Retune: alongside the manifest RSS expectation.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Fixed stack size for the dedicated syn parse thread. Pinning the stack
/// makes the crash threshold independent of the inherited environment stack
/// (the plugin's main thread has whatever the host/OS gave it). 16 MiB doubles
/// the common 8 MiB default, putting every measured crash floor ≥4x above the
/// scan caps. Retune together with the caps above.
pub const PARSE_STACK_BYTES: usize = 16 * 1024 * 1024;

/// A pre-parse guard rejection. Each variant maps to a degraded module entity
/// plus one warning finding (`LMWV-RUST-DEPTH-LIMIT` /
/// `LMWV-RUST-FILE-TOO-LARGE`) rather than a parse attempt that could abort
/// the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardViolation {
    /// Bracket nesting exceeded [`MAX_BRACKET_DEPTH`]; `depth` is the depth at
    /// the trip point (cap + 1).
    Depth {
        /// Nesting depth at the trip point.
        depth: usize,
    },
    /// A consecutive prefix-operator run exceeded [`MAX_PREFIX_RUN`]; `len` is
    /// the run length at the trip point (cap + 1).
    PrefixRun {
        /// Run length at the trip point.
        len: usize,
    },
    /// The file's on-disk size exceeds [`MAX_FILE_BYTES`].
    FileTooLarge {
        /// File size reported by `fs::metadata`.
        bytes: u64,
    },
}

impl GuardViolation {
    /// The `parse_status` value the degraded module entity carries.
    #[must_use]
    pub fn parse_status(&self) -> &'static str {
        match self {
            Self::Depth { .. } | Self::PrefixRun { .. } => "depth_limit",
            Self::FileTooLarge { .. } => "file_too_large",
        }
    }

    /// The finding subcode (under the manifest's `LMWV-RUST-` prefix).
    #[must_use]
    pub fn subcode(&self) -> &'static str {
        match self {
            Self::Depth { .. } | Self::PrefixRun { .. } => "LMWV-RUST-DEPTH-LIMIT",
            Self::FileTooLarge { .. } => "LMWV-RUST-FILE-TOO-LARGE",
        }
    }

    /// Human-readable finding message naming the measurement and the cap.
    #[must_use]
    pub fn message(&self, file_path: &str) -> String {
        match self {
            Self::Depth { depth } => format!(
                "{file_path}: bracket nesting depth {depth} exceeds the cap \
                 ({MAX_BRACKET_DEPTH}); skipped to avoid a parser stack overflow"
            ),
            Self::PrefixRun { len } => format!(
                "{file_path}: prefix-operator run of {len} exceeds the cap \
                 ({MAX_PREFIX_RUN}); skipped to avoid a parser stack overflow"
            ),
            Self::FileTooLarge { bytes } => format!(
                "{file_path}: file size {bytes} bytes exceeds the cap \
                 ({MAX_FILE_BYTES} bytes); skipped to bound plugin memory"
            ),
        }
    }
}

/// Reject a file whose on-disk size exceeds [`MAX_FILE_BYTES`] WITHOUT reading
/// it. An unreadable path passes (the read that follows surfaces the error the
/// same way it does today).
///
/// # Errors
///
/// [`GuardViolation::FileTooLarge`] when `fs::metadata` reports more than
/// [`MAX_FILE_BYTES`] bytes.
pub fn check_file_size(path: &Path) -> Result<(), GuardViolation> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => {
            Err(GuardViolation::FileTooLarge { bytes: meta.len() })
        }
        _ => Ok(()),
    }
}

/// Scan `src` with the minimal lexer and reject bracket-depth / prefix-run
/// bombs before they reach syn. O(n), byte-oriented, zero allocation.
///
/// # Errors
///
/// [`GuardViolation::Depth`] when bracket nesting exceeds
/// [`MAX_BRACKET_DEPTH`]; [`GuardViolation::PrefixRun`] when a consecutive
/// prefix-operator run exceeds [`MAX_PREFIX_RUN`].
pub fn scan_source(src: &str) -> Result<(), GuardViolation> {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut depth: usize = 0;
    let mut run: usize = 0;
    let mut i = 0;
    while i < n {
        let b = bytes[i];
        // Comments first: their contents (brackets, banner asterisks) are
        // invisible to both counters. The prefix run survives a comment —
        // like whitespace, a comment between prefix ops is transparent to
        // syn's expression recursion (`!/**/!/**/…` is still one chain).
        if b == b'/' && i + 1 < n {
            match bytes[i + 1] {
                b'/' => {
                    i += 2;
                    while i < n && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'*' => {
                    i = skip_block_comment(bytes, i + 2);
                    continue;
                }
                _ => {}
            }
        }
        // Raw strings (`r"…"`, `r#"…"#`, `br…`, `cr…`) need lookahead BEFORE
        // the plain-`"` arm, because their bodies escape nothing and may hold
        // unescaped quotes. A raw IDENTIFIER (`r#match`) falls through (no
        // opening quote after the hashes).
        if matches!(b, b'r' | b'b' | b'c')
            && !prev_is_ident(bytes, i)
            && let Some(next) = skip_raw_string(bytes, i)
        {
            i = next;
            run = 0;
            continue;
        }
        match b {
            b'"' => {
                i = skip_string(bytes, i + 1);
                run = 0;
                continue;
            }
            b'\'' => {
                // Char literal vs lifetime tick: `'\…'` and `'x'` are
                // literals; anything else (`'a`, `'static`, a loop label) is a
                // tick to step over in Normal state.
                if let Some(next) = skip_char_literal(bytes, i) {
                    i = next;
                } else {
                    i += 1;
                }
                run = 0;
                continue;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > MAX_BRACKET_DEPTH {
                    return Err(GuardViolation::Depth { depth });
                }
                run = 0;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                run = 0;
            }
            b'!' | b'&' | b'*' | b'-' => {
                run += 1;
                if run > MAX_PREFIX_RUN {
                    return Err(GuardViolation::PrefixRun { len: run });
                }
            }
            // Whitespace does not launder a prefix run: `! ! !…1` recurses in
            // syn exactly like `!!!…1`.
            b' ' | b'\t' | b'\n' | b'\r' => {}
            _ => run = 0,
        }
        i += 1;
    }
    Ok(())
}

/// Whether the byte before `i` can end an identifier (blocks raw-string
/// lookahead inside identifiers like `var"` or `attr` ending in `r`).
fn prev_is_ident(bytes: &[u8], i: usize) -> bool {
    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')
}

/// Skip a (nesting) block comment whose `/*` has been consumed; `i` points at
/// the first content byte. Returns the index just past the closing `*/` (or
/// `bytes.len()` for an unterminated comment — syn will reject the file).
fn skip_block_comment(bytes: &[u8], mut i: usize) -> usize {
    let n = bytes.len();
    let mut depth = 1usize;
    while i < n && depth > 0 {
        if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// Skip a plain string literal whose opening `"` has been consumed; `j` points
/// at the first content byte. `\` escapes the next byte (so `\"` is content).
/// Returns the index just past the closing quote (or past EOF when
/// unterminated — the caller's loop bound handles it).
fn skip_string(bytes: &[u8], mut j: usize) -> usize {
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    j
}

/// Lookahead for a raw-string literal starting at `i` (which holds `r`, `b`,
/// or `c`): optional `b`/`c`, then `r`, then `#`*N, then `"`. Returns the
/// index just past the closing `"` + N `#`s, or `None` when this is not a raw
/// string (e.g. the identifier `r`, or a raw identifier `r#match`). An
/// unterminated raw string consumes to EOF.
fn skip_raw_string(bytes: &[u8], i: usize) -> Option<usize> {
    let n = bytes.len();
    let mut j = i;
    if matches!(bytes[j], b'b' | b'c') {
        j += 1;
    }
    if j >= n || bytes[j] != b'r' {
        return None;
    }
    j += 1;
    let mut hashes = 0usize;
    while j < n && bytes[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j >= n || bytes[j] != b'"' {
        return None;
    }
    j += 1;
    while j < n {
        if bytes[j] == b'"'
            && n - (j + 1) >= hashes
            && bytes[j + 1..j + 1 + hashes].iter().all(|&h| h == b'#')
        {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(n)
}

/// Lookahead for a char LITERAL at `i` (which holds `'`). Returns the index
/// just past the closing quote, or `None` when the tick is a lifetime/label
/// (`'a`, `'static`, `'outer:`) — i.e. not followed by an escape and not of
/// the two-byte `'x'` shape.
fn skip_char_literal(bytes: &[u8], i: usize) -> Option<usize> {
    let n = bytes.len();
    if i + 1 >= n {
        return None;
    }
    if bytes[i + 1] == b'\\' {
        // Escaped literal (`'\''`, `'\u{1F600}'`): scan to the closing quote.
        let mut j = i + 2;
        while j < n {
            match bytes[j] {
                b'\\' => j += 2,
                b'\'' => return Some(j + 1),
                _ => j += 1,
            }
        }
        return Some(n);
    }
    if i + 2 < n && bytes[i + 2] == b'\'' && bytes[i + 1] != b'\'' {
        return Some(i + 3); // 'x'
    }
    None
}

/// Run `f` (a syn parse plus whatever consumes the AST) on a dedicated scoped
/// thread with a fixed [`PARSE_STACK_BYTES`] stack, so the crash threshold the
/// scan caps were tuned against does not depend on the inherited environment
/// stack.
///
/// A closure-runner rather than a `parse → syn::File` helper because
/// `syn::File` is `!Send` (`proc_macro2` spans are thread-bound): the AST cannot
/// cross the thread boundary, so the recursive *consumers* of the AST (the
/// extraction walk) must run on the pinned stack too — which is also what we
/// want, since they recurse over the same nested structure.
///
/// **Fallback:** the host jails the plugin child with `RLIMIT_NPROC` (a
/// per-real-UID-GLOBAL task counter — ADR-021 §2d), so on a busy user account
/// `clone(2)` fails with `EAGAIN` and the pinned thread cannot be spawned at
/// all. In that case `f` runs INLINE on the calling thread: the scan caps
/// ([`MAX_BRACKET_DEPTH`]/[`MAX_PREFIX_RUN`]) sit well under the measured
/// crash floors even at the common 8 MiB default stack (337/2386), so guarded
/// input stays safe; the pinned thread is defence-in-depth where the
/// environment allows it, never a correctness dependency.
///
/// # Panics
///
/// Panics if the spawned parse thread itself panics — an infrastructure
/// fault, not an input-dependent condition (guarded input cannot overflow the
/// pinned stack).
pub fn with_pinned_stack<T, F>(f: F) -> T
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    // The closure is parked in a Mutex<Option<…>> so it survives a failed
    // spawn: `Builder::spawn_scoped` consumes its argument, which would drop
    // `f` on the EAGAIN path before the inline fallback could run it.
    let parked = std::sync::Mutex::new(Some(f));
    let take = || {
        parked
            .lock()
            .expect("parse-closure mutex poisoned")
            .take()
            .expect("parse closure runs exactly once")
    };
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name("syn-parse".into())
            .stack_size(PARSE_STACK_BYTES)
            .spawn_scoped(scope, || take()());
        match spawned {
            Ok(handle) => handle.join().expect("syn parse thread panicked"),
            Err(e) => {
                // EAGAIN under RLIMIT_NPROC: inline fallback. Warn once so the
                // host's stderr ring buffer records which stack is live.
                static WARN_ONCE: std::sync::Once = std::sync::Once::new();
                WARN_ONCE.call_once(|| {
                    eprintln!(
                        "loomweave-plugin-rust: cannot spawn pinned parse thread ({e}); \
                         parsing inline on the caller's stack (scan caps still apply)"
                    );
                });
                take()()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `fn f() { let _ = <open*n>core<close*n>; }` — the bracket-bomb shape the
    /// empirical probe used. Generated in-test, never checked in (plan rule).
    fn nested(open: &str, close: &str, n: usize, core: &str) -> String {
        format!(
            "fn f() {{ let _ = {}{}{}; }}",
            open.repeat(n),
            core,
            close.repeat(n)
        )
    }

    #[test]
    fn depth_under_cap_passes() {
        // 127 nested parens + the fn's own braces stays at/under the cap.
        assert_eq!(scan_source(&nested("(", ")", 126, "1")), Ok(()));
    }

    #[test]
    fn depth_over_cap_trips_and_reports_trip_depth() {
        assert_eq!(
            scan_source(&nested("(", ")", 200, "1")),
            Err(GuardViolation::Depth {
                depth: MAX_BRACKET_DEPTH + 1
            })
        );
    }

    #[test]
    fn square_and_curly_brackets_count_toward_depth() {
        assert!(matches!(
            scan_source(&nested("[", "]", 200, "1")),
            Err(GuardViolation::Depth { .. })
        ));
        assert!(matches!(
            scan_source(&nested("{", "}", 200, "1")),
            Err(GuardViolation::Depth { .. })
        ));
    }

    #[test]
    fn unbalanced_closers_saturate_instead_of_underflowing() {
        assert_eq!(scan_source(")))))) fn f() {}"), Ok(()));
    }

    #[test]
    fn prefix_bomb_trips() {
        let src = format!("fn f() {{ let _ = {}1; }}", "!".repeat(2000));
        assert_eq!(
            scan_source(&src),
            Err(GuardViolation::PrefixRun {
                len: MAX_PREFIX_RUN + 1
            })
        );
    }

    #[test]
    fn mixed_prefix_operator_run_trips() {
        // The run counter is shared across the prefix set, so `&*&*…` (a
        // deref/ref bomb) trips just like `!!!…`.
        let src = format!("fn f() {{ let _ = {}x; }}", "&*".repeat(1000));
        assert!(matches!(
            scan_source(&src),
            Err(GuardViolation::PrefixRun { .. })
        ));
    }

    #[test]
    fn prefix_run_resets_on_other_bytes() {
        // 800 + 800 with a reset between never exceeds the 1024 cap.
        let src = format!(
            "fn f() {{ let _ = {}1; let _ = {}2; }}",
            "!".repeat(800),
            "!".repeat(800)
        );
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn whitespace_interleaved_prefix_bomb_trips() {
        // `! ! ! … 1` is the same recursive unary chain to syn as `!!!…1`;
        // whitespace must not launder the run.
        let src = format!("fn f() {{ let _ = {}1; }}", "! ".repeat(2000));
        assert!(matches!(
            scan_source(&src),
            Err(GuardViolation::PrefixRun { .. })
        ));
    }

    #[test]
    fn comment_interleaved_prefix_bomb_trips() {
        // Block comments between prefix ops are likewise transparent to syn's
        // expression recursion; they must not launder the run either.
        let src = format!("fn f() {{ let _ = {}1; }}", "!/**/".repeat(2000));
        assert!(matches!(
            scan_source(&src),
            Err(GuardViolation::PrefixRun { .. })
        ));
    }

    #[test]
    fn banner_comment_brackets_are_skipped() {
        let src = format!("/* {} */\nfn f() {{}}\n", "(".repeat(300));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn banner_comment_asterisks_do_not_trip_the_prefix_run() {
        // `*` is a prefix-run byte; a 2000-star banner comment must NOT trip.
        let src = format!("/{}/\nfn f() {{}}\n", "*".repeat(2000));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn line_comment_contents_are_skipped() {
        let src = format!("// {}{}\nfn f() {{}}\n", "(".repeat(300), "!".repeat(2000));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn nested_block_comments_are_skipped_entirely() {
        // Rust block comments nest: the inner `*/` must not end the outer
        // comment and expose the bracket payload.
        let src = format!("/* outer /* inner */ {} */\nfn f() {{}}\n", "[".repeat(300));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn string_literal_brackets_are_skipped() {
        let src = format!("fn f() {{ let _ = \"{}\"; }}", "[".repeat(300));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn string_escaped_quote_does_not_end_the_string() {
        let src = format!("fn f() {{ let _ = \"a\\\"{}\"; }}", "(".repeat(300));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn raw_string_brackets_are_skipped() {
        let src = "fn f() { let _ = r#\"((((((\"# ; }".to_owned()
            + &format!("\nfn g() {{ let _ = r#\"{}\"#; }}", "(".repeat(300));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn raw_string_inner_quote_without_hashes_does_not_terminate() {
        // r##"…"…"## — a `"` followed by only one `#` is content, not the end.
        let src = format!(
            "fn f() {{ let _ = r##\"{}\"#{}\"##; }}",
            "(".repeat(200),
            "[".repeat(200)
        );
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn byte_and_c_raw_strings_are_skipped() {
        let src = format!(
            "fn f() {{ let _ = br#\"{}\"#; let _ = cr#\"{}\"#; }}",
            "(".repeat(300),
            "[".repeat(300)
        );
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn raw_identifier_is_not_a_raw_string() {
        // `r#match` is a raw IDENTIFIER; the lexer must not eat the rest of
        // the file looking for a closing quote (which would hide a bomb… or
        // here, hide nothing — the bomb after it must still trip).
        let src = format!(
            "fn f() {{ let r#match = 1; let _ = {}1{}; }}",
            "(".repeat(200),
            ")".repeat(200)
        );
        assert!(matches!(
            scan_source(&src),
            Err(GuardViolation::Depth { .. })
        ));
    }

    #[test]
    fn char_literal_bracket_is_not_counted() {
        // 200 separate `'('` char literals: without char-literal handling each
        // `(` would count as an unclosed bracket and trip the depth cap.
        let src = format!("fn f() {{ let _ = [{}]; }}", "'(',".repeat(200));
        assert_eq!(scan_source(&src), Ok(()));
    }

    #[test]
    fn escaped_char_literal_is_skipped() {
        let src = "fn f() { let _ = '\\''; let _ = '\\u{1F600}'; let _ = '\\\\'; }";
        assert_eq!(scan_source(src), Ok(()));
    }

    #[test]
    fn lifetimes_are_not_char_literals() {
        // `'a` must not open a char literal that swallows the following bomb.
        let src = format!(
            "fn f<'a>(x: &'a str) -> &'a str {{ x }}\nfn g() {{ let _ = {}1{}; }}",
            "(".repeat(200),
            ")".repeat(200)
        );
        assert!(matches!(
            scan_source(&src),
            Err(GuardViolation::Depth { .. })
        ));
    }

    #[test]
    fn benign_real_source_passes() {
        // This very file is a benign real-world source: it must pass.
        assert_eq!(scan_source(include_str!("parse_guard.rs")), Ok(()));
    }

    #[test]
    fn pinned_stack_parses_depth_a_default_thread_stack_might_not() {
        // 120 nested mods ≈ 3 MiB of syn frames: would abort a 2 MiB thread,
        // proving the parse really runs on the 16 MiB pinned stack. The AST is
        // !Send, so it is consumed (item count) on the pinned thread.
        let n = 120;
        let src = format!("{}fn leaf() {{}}{}", "mod m {".repeat(n), "}".repeat(n));
        let items = with_pinned_stack(|| syn::parse_file(&src).map(|f| f.items.len()))
            .expect("pinned stack must absorb depth 120");
        assert_eq!(items, 1);
    }

    #[test]
    fn pinned_stack_surfaces_syntax_errors() {
        // syn::Error IS Send (syn's ThreadBound wrapper), so the error crosses.
        assert!(with_pinned_stack(|| syn::parse_file("fn broken( {{{ not rust").err()).is_some());
    }

    #[test]
    fn file_size_cap_trips_on_oversize_file_without_reading_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.rs");
        // A sparse file: metadata reports the size with no 11 MiB write.
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(MAX_FILE_BYTES + 1).expect("set_len");
        assert_eq!(
            check_file_size(&path),
            Err(GuardViolation::FileTooLarge {
                bytes: MAX_FILE_BYTES + 1
            })
        );
    }

    #[test]
    fn file_size_cap_passes_small_and_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.rs");
        std::fs::write(&path, "fn f() {}\n").expect("write");
        assert_eq!(check_file_size(&path), Ok(()));
        // Missing file passes: the read that follows owns that error path.
        assert_eq!(check_file_size(&dir.path().join("absent.rs")), Ok(()));
    }

    #[test]
    fn violation_metadata_maps_to_subcodes_and_statuses() {
        let d = GuardViolation::Depth { depth: 129 };
        assert_eq!(d.parse_status(), "depth_limit");
        assert_eq!(d.subcode(), "LMWV-RUST-DEPTH-LIMIT");
        assert!(d.message("a.rs").contains("129"));
        let p = GuardViolation::PrefixRun { len: 1025 };
        assert_eq!(p.parse_status(), "depth_limit");
        assert_eq!(p.subcode(), "LMWV-RUST-DEPTH-LIMIT");
        let f = GuardViolation::FileTooLarge { bytes: 11 << 20 };
        assert_eq!(f.parse_status(), "file_too_large");
        assert_eq!(f.subcode(), "LMWV-RUST-FILE-TOO-LARGE");
        assert!(f.message("b.rs").contains(&(11_u64 << 20).to_string()));
    }
}
