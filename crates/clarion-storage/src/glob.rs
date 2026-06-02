//! Path glob matching shared across the read (MCP `scope` / guidance
//! `match_rules`) and write (CLI `guidance --for-entity`) surfaces.
//!
//! Lifted into the storage crate so a single implementation backs both the MCP
//! catalogue (which historically owned it as `catalogue::glob_match`) and the
//! CLI guidance authoring path — one matcher, no drift. `clarion-mcp`
//! re-exports this function so its semantics are unchanged.

/// Glob-match `path` against a `**`/`*`/`?` `pattern`, treating `/` as the
/// path separator. `**` matches zero or more whole segments; `*` matches any
/// run of non-`/` characters within a single segment; `?` matches one such
/// character. Used by `scope` path-globs and by guidance `path` match-rules.
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let seg: Vec<&str> = path.split('/').collect();
    glob_segments(&pat, &seg)
}

fn glob_segments(pat: &[&str], seg: &[&str]) -> bool {
    match pat.first() {
        None => seg.is_empty(),
        Some(&"**") => {
            // `**` consumes zero or more whole segments; try each split point.
            (0..=seg.len()).any(|i| glob_segments(&pat[1..], &seg[i..]))
        }
        Some(head) => match seg.first() {
            Some(name) if segment_match(head.as_bytes(), name.as_bytes()) => {
                glob_segments(&pat[1..], &seg[1..])
            }
            _ => false,
        },
    }
}

/// Within-segment wildcard match: `*` matches any run, `?` matches one char.
fn segment_match(pat: &[u8], name: &[u8]) -> bool {
    match pat.first() {
        None => name.is_empty(),
        Some(b'*') => {
            // `*` matches zero or more chars within the segment.
            (0..=name.len()).any(|i| segment_match(&pat[1..], &name[i..]))
        }
        Some(b'?') => match name.first() {
            Some(_) => segment_match(&pat[1..], &name[1..]),
            None => false,
        },
        Some(&head) => match name.first() {
            Some(&c) if c == head => segment_match(&pat[1..], &name[1..]),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn double_star_across_segments() {
        assert!(glob_match("src/auth/**", "src/auth/tokens/refresh.py"));
        assert!(glob_match("src/auth/**", "src/auth/mod.py"));
        assert!(glob_match("src/**", "src/auth/tokens/refresh.py"));
        assert!(glob_match("**/refresh.py", "src/auth/refresh.py"));
    }

    #[test]
    fn single_star_stays_within_segment() {
        assert!(glob_match("src/*.py", "src/main.py"));
        assert!(!glob_match("src/*.py", "src/auth/main.py"));
        assert!(glob_match("src/auth/*.py", "src/auth/tokens.py"));
    }

    #[test]
    fn rejects_non_matches() {
        assert!(!glob_match("src/auth/**", "src/billing/tokens.py"));
        assert!(!glob_match("src/auth/tokens.py", "src/auth/sessions.py"));
    }

    #[test]
    fn question_matches_single_char() {
        assert!(glob_match("src/v?.py", "src/v1.py"));
        assert!(!glob_match("src/v?.py", "src/v10.py"));
    }
}
