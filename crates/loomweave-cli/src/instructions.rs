//! Loomweave-owned agent-orientation block injected into `CLAUDE.md` /
//! `AGENTS.md`, plus its idempotent installer and read-only health check.
//!
//! Like Filigree, Loomweave *pushes* a small managed marker-block into the
//! always-loaded `CLAUDE.md` / `AGENTS.md` context so an agent learns to ask
//! Loomweave's MCP tools before re-grepping the tree. Unlike the skill pack
//! (whose asset is owned by `loomweave-mcp`), this asset is cli-local — there
//! is no MCP owner for it — and is embedded with `include_str!`, matching the
//! embedding convention in [`crate::skill_pack`].
//!
//! ## Coexistence is the whole point
//!
//! Every file Loomweave writes here **already** contains another tool's block:
//! this repo's own `AGENTS.md` holds Filigree's `<!-- filigree:instructions -->`
//! span (and Wardline's). Loomweave therefore *never* owns the tail of the file,
//! so the installer must touch **only** its own
//! `<!-- loomweave:instructions -->`…`<!-- /loomweave:instructions -->` span and
//! must not delete or move a single byte outside it. In particular it does NOT
//! copy Filigree's truncate-from-start-marker-to-EOF malformed recovery, which
//! is a data-loss bug in a two-block file. See [`install_instructions`].
//!
//! ## Drift signal
//!
//! Drift is the block-body content plus the stable last-writer metadata compared
//! byte-for-byte against the embedded [`INSTRUCTIONS_BODY`], **not** the marker
//! version string — so a workspace version bump on byte-identical content does
//! not report drift. This mirrors [`crate::skill_pack`]'s fingerprint
//! philosophy; the `v{version}` in the start marker is human-readable
//! provenance only.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Embedded, cli-local instructions body. Deliberately thin: it is
/// always-loaded context competing with the `loomweave-workflow` skill that
/// says the same thing, so it is a pointer, not a manual.
const INSTRUCTIONS_BODY: &str = include_str!("../assets/instructions/loomweave.md");
const WRITER_MARKER: &str = "<!-- loomweave:last-writer:loomweave install -->";

/// Detection prefix for Loomweave's start marker. The full marker carries a
/// `:v{version}:{hash}` provenance suffix (see [`start_marker`]); detection
/// keys only on this prefix so a provenance change is still recognised as the
/// same block. Never collides with `<!-- filigree:instructions` or
/// `<!-- wardline:instructions` — those are different tool namespaces.
const START_PREFIX: &str = "<!-- loomweave:instructions";

/// Loomweave's end marker, matched as a whole trimmed line.
const END_MARKER: &str = "<!-- /loomweave:instructions -->";

/// The two project-root files Loomweave manages a block in.
const TARGET_FILES: &[&str] = &[CLAUDE_MD, AGENTS_MD];

/// Claude Code's project-root agent-context file.
const CLAUDE_MD: &str = "CLAUDE.md";

/// The tool-agnostic project-root agent-context file, and the redirect target.
const AGENTS_MD: &str = "AGENTS.md";

/// The canonical body bytes that live inside the span. `include_str!` keeps the
/// asset's trailing newline; we trim trailing whitespace so the drift compare
/// is invariant to how the asset file happens to end. This is the single source
/// of truth for both render ([`render_block`]) and extract ([`locate_span`]).
fn canonical_body() -> String {
    format!("{}\n{}", WRITER_MARKER, INSTRUCTIONS_BODY.trim_end())
}

/// First 8 hex chars of the blake3 digest over [`canonical_body`] — provenance
/// only, stamped into the start marker; not the drift signal.
fn body_hash_prefix() -> String {
    let digest = blake3::hash(canonical_body().as_bytes());
    digest.to_hex()[..8].to_owned()
}

/// The full provenance start-marker line (no trailing newline).
fn start_marker() -> String {
    format!(
        "<!-- loomweave:instructions:v{}:{} -->",
        env!("CARGO_PKG_VERSION"),
        body_hash_prefix()
    )
}

/// Render the complete block (start marker + body + end marker), newline-pinned.
///
/// Exactly one newline sits at each boundary: after the start marker, between
/// the body and the end marker. [`locate_span`] is the precise inverse, so a
/// freshly rendered block round-trips to [`canonical_body`] with no drift.
fn render_block() -> String {
    format!("{}\n{}\n{}", start_marker(), canonical_body(), END_MARKER)
}

/// Read-only health of the Loomweave block across both [`TARGET_FILES`], for
/// `loomweave doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionsState {
    /// Every target file holds a well-formed block whose body matches the
    /// embedded bytes.
    UpToDate,
    /// At least one target file is missing the block entirely (and no file is
    /// in a worse state). A first-class but *optional* surface: the same
    /// guidance is delivered by the MCP preamble and the skill, so a project
    /// that omits the block is still healthy. Doctor treats this as a
    /// **warning**.
    Missing,
    /// Every file that should hold a block has a well-formed one, but at least
    /// one block's body differs from the embedded bytes (a stale copy from an
    /// older binary, or hand-edited). Doctor treats this as a **problem**
    /// (auto-repaired with `--fix`).
    Drifted,
    /// At least one target file has a malformed block — a dangling start marker
    /// with no following end marker, an end marker preceding its start, or an own
    /// close marker that lies *beyond* a co-resident foreign block (so a naive
    /// open..close match would swallow the sibling). Doctor treats this as a
    /// **problem**; the repair is safe because it only rewrites Loomweave's own
    /// span, bounded at the first foreign fence.
    Malformed,
    /// Every file is well-formed, but at least one carries a stale **duplicate**
    /// own block — a second Loomweave block before any foreign fence, or one
    /// shielded beyond a foreign block. A split-brain problem (the duplicate is
    /// silent, conflicting guidance). Doctor treats this as a **problem**: the
    /// canonicalisable case is auto-collapsed with `--fix`; a foreign-shielded
    /// duplicate is surfaced for hand resolution (foreign-safety > own-dedup).
    Duplicated,
    /// `CLAUDE.md` only redirects to `AGENTS.md` (C-20), yet still carries a
    /// legacy Loomweave block. Under a redirect the healthy state is the block's
    /// **absence** here — the redirect already delivers it from `AGENTS.md`, so a
    /// second copy is duplicate guidance free to drift. Doctor treats this as a
    /// **problem** whose repair is *migration*, never re-injection.
    RedirectStale,
}

/// Classify one file's Loomweave block without writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    /// No start marker present.
    Absent,
    /// Well-formed block whose body matches the embedded bytes.
    Current,
    /// Well-formed block whose body differs from the embedded bytes.
    Drifted,
    /// Start marker present without a following end marker, markers mis-ordered,
    /// or the own close marker lies beyond a co-resident foreign block.
    Malformed,
    /// Well-formed first own block, but a stale duplicate own block also exists
    /// (canonicalisable before a foreign fence, or shielded beyond one).
    Duplicated,
    /// A migrate-off target (a redirecting `CLAUDE.md`) that still carries an own
    /// block. Healthy for such a file is *no* own block at all.
    Leftover,
}

/// Aggregate per-file states into a single [`InstructionsState`].
///
/// Precedence is **severity-ordered**, high → low: `Malformed` > `Drifted` >
/// `Duplicated` > `Leftover` > `Missing` > `UpToDate`. This deliberately differs
/// from [`crate::skill_pack`]'s "Missing first" rule: here `Missing` is only a
/// warning while every state above it fails the gate, so a missing block must
/// never mask a gate-failing one.
fn aggregate(states: &[FileState]) -> InstructionsState {
    if states.iter().any(|s| matches!(s, FileState::Malformed)) {
        InstructionsState::Malformed
    } else if states.iter().any(|s| matches!(s, FileState::Drifted)) {
        InstructionsState::Drifted
    } else if states.iter().any(|s| matches!(s, FileState::Duplicated)) {
        InstructionsState::Duplicated
    } else if states.iter().any(|s| matches!(s, FileState::Leftover)) {
        InstructionsState::RedirectStale
    } else if states.iter().any(|s| matches!(s, FileState::Absent)) {
        InstructionsState::Missing
    } else {
        InstructionsState::UpToDate
    }
}

/// Classify the Loomweave block across a project's agent-context files without
/// writing.
///
/// Redirect-aware (C-20): the file set and the *meaning* of each file both come
/// from [`instruction_targets`]. Under a redirect, `CLAUDE.md` is judged by
/// [`migrated_file_state`] — where absence is health — so a redirecting project
/// is never reported `Missing` for a block that deliberately does not live
/// there.
#[must_use]
pub fn instructions_state(project_root: &Path) -> InstructionsState {
    let (write_to, migrate_off) = instruction_targets(project_root);
    let states: Vec<FileState> = write_to
        .iter()
        .map(|name| file_state(&project_root.join(name)))
        .chain(
            migrate_off
                .iter()
                .map(|name| migrated_file_state(&project_root.join(name))),
        )
        .collect();
    aggregate(&states)
}

/// Classify a file the block should have been migrated *off*. Healthy is the
/// absence of an own block; an unreadable or absent file is healthy too (there
/// is certainly no block in it).
///
/// Keyed on *any* own open-marker line, not just a claimable top-level one: a
/// marker shielded inside an unclosed foreign block is still stale guidance the
/// redirect makes redundant, and reporting it (as unfixable, since
/// [`remove_instructions`] will not claim what it cannot prove is ours) beats
/// going green over a split brain.
fn migrated_file_state(path: &Path) -> FileState {
    let Ok(content) = fs::read_to_string(path) else {
        return FileState::Current;
    };
    if own_open_marker_count(&content) == 0 {
        FileState::Current
    } else {
        FileState::Leftover
    }
}

/// Classify a single target file. A file that does not exist is [`Absent`]
/// (the installer will create it); an unreadable file is treated as `Absent`
/// too, so the repair path attempts a fresh write rather than wedging.
///
/// [`Absent`]: FileState::Absent
fn file_state(path: &Path) -> FileState {
    let Ok(content) = fs::read_to_string(path) else {
        return FileState::Absent;
    };
    match locate_span(&content) {
        Span::Absent => FileState::Absent,
        Span::Malformed => FileState::Malformed,
        Span::WellFormed { end, body } => {
            // A stale duplicate own block — either canonicalisable (before any
            // real foreign block) or shielded (beyond one) — is a split-brain
            // problem, not health (C-4 (e)). Check before the drift compare so a
            // body-current first block can never green over a duplicate.
            let tail = &content[end..];
            let foreign = first_real_foreign_block_pos(tail, 0);
            let head = &tail[..foreign];
            let canonicalisable_dup = remove_own_blocks(head).as_str() != head;
            let shielded_dup = first_own_open_fence_pos(&tail[foreign..]).is_some();
            if canonicalisable_dup || shielded_dup {
                FileState::Duplicated
            } else if body == canonical_body() {
                FileState::Current
            } else {
                FileState::Drifted
            }
        }
    }
}

/// Loomweave's own vendor namespace, lower-cased for the case-insensitive
/// fence comparison mandated by C-4 clause (h).
const OWN_NS: &str = "loomweave";

/// Where (and whether) a well-ordered Loomweave block sits in `content`.
enum Span {
    /// No own top-level start marker present.
    Absent,
    /// Start marker present without a following end marker, mis-ordered, or the
    /// own close marker lies beyond a co-resident foreign block.
    Malformed,
    /// A well-ordered block whose close precedes any real foreign block. `end`
    /// is the byte offset just past the end-marker line (including its trailing
    /// newline if any). `body` is the extracted block body, trailing-newline-
    /// trimmed, for the drift compare.
    WellFormed { end: usize, body: String },
}

/// A managed-block fence (open or close) recognised on its own line.
struct Fence {
    /// Lower-cased vendor namespace (C-4 (h): compared case-insensitively).
    ns: String,
    /// True for a close fence (`<!-- /<ns>:instructions … -->`).
    is_close: bool,
    /// Byte offset of the start of the line carrying the fence.
    pos: usize,
}

/// Parse an already-trimmed line as a managed-block fence, returning its
/// lower-cased namespace and whether it is a close fence. Recognises
/// `<!-- [/]<ns>:instructions …`, namespace charset `[A-Za-z0-9_-]+` matched
/// case-insensitively (C-4 clause (h)). This is the namespace-fence detector
/// that lets foreign boundaries — including differently-cased siblings —
/// register, and is the prerequisite primitive for bounded recovery (c) and
/// own-duplicate canonicalisation (e).
fn parse_fence(trimmed: &str) -> Option<(String, bool)> {
    let rest = trimmed.strip_prefix("<!--")?.trim_start();
    let (is_close, rest) = rest.strip_prefix('/').map_or((false, rest), |r| (true, r));
    let ns_len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        .count();
    if ns_len == 0 || !rest[ns_len..].starts_with(":instructions") {
        return None;
    }
    Some((rest[..ns_len].to_ascii_lowercase(), is_close))
}

/// Every managed-block fence in `content`, in document order, line-anchored —
/// never a bare `-->` substring scan, which could match a sibling's marker
/// mid-prose.
fn fences(content: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if let Some((ns, is_close)) = parse_fence(line.trim()) {
            out.push(Fence {
                ns,
                is_close,
                pos: line_start,
            });
        }
    }
    out
}

/// Byte offset of the first *real* foreign block at/after `search_from`, else
/// `content.len()` (bound at EOF). A real foreign block is a foreign-namespace
/// OPEN fence with a matching foreign CLOSE fence after it — genuine co-resident
/// sibling content we must never delete or split (C-4 (c)). A lone foreign open
/// (a marker quoted in prose or inside our own body) and a stray foreign close
/// are NOT boundaries: a well-formed own block whose body merely mentions a
/// sibling's marker is replaced in place, not truncated at the quoted marker.
/// Own-namespace fences are always absorbed, so duplicate/unclosed own blocks
/// still collapse.
fn first_real_foreign_block_pos(content: &str, search_from: usize) -> usize {
    let all = fences(content);
    let relevant: Vec<&Fence> = all.iter().filter(|f| f.pos >= search_from).collect();
    for (i, f) in relevant.iter().enumerate() {
        if f.ns == OWN_NS || f.is_close {
            continue;
        }
        if relevant[i + 1..].iter().any(|n| n.ns == f.ns && n.is_close) {
            return f.pos;
        }
    }
    content.len()
}

/// Byte offset of Loomweave's own *top-level* open fence, or `None`. An own open
/// marker quoted inside an (unclosed) foreign block is shielded — we decline to
/// claim content we cannot prove is ours, and the caller falls back to an append
/// (which deletes nothing). This is the foreign-safe anchor for the replace path.
fn first_own_open_fence_pos(content: &str) -> Option<usize> {
    let mut inside_foreign: Option<String> = None;
    for f in fences(content) {
        match &inside_foreign {
            Some(ns) => {
                if f.is_close && &f.ns == ns {
                    inside_foreign = None;
                }
            }
            None => {
                if !f.is_close {
                    if f.ns == OWN_NS {
                        return Some(f.pos);
                    }
                    inside_foreign = Some(f.ns);
                }
            }
        }
    }
    None
}

/// `(line_start, line_end)` of the first line strictly after `after` whose
/// trimmed form equals [`END_MARKER`]; `None` if none follows. Whole-line
/// matched so it never trips on a sibling's `-->`.
fn own_end_line_pos(content: &str, after: usize) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;
        if line_start > after && line.trim() == END_MARKER {
            return Some((line_start, line_end));
        }
    }
    None
}

/// Remove every *complete* own block (own start line through its own end line,
/// inclusive) from `text`, keeping all other bytes verbatim. A dangling own open
/// with no following close is left in place (mirrors a non-greedy open..close
/// substitution). Used to canonicalise duplicate own blocks in a region already
/// known to be free of real foreign blocks.
fn remove_own_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut buf = String::new();
    let mut in_own = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if in_own {
            buf.push_str(line);
            if trimmed == END_MARKER {
                in_own = false;
                buf.clear(); // complete own block — drop it
            }
        } else if trimmed.starts_with(START_PREFIX) {
            in_own = true;
            buf.clear();
            buf.push_str(line);
        } else {
            out.push_str(line);
        }
    }
    out.push_str(&buf); // dangling own open with no close: keep verbatim
    out
}

/// Collapse duplicate own blocks in `tail` that precede the first real foreign
/// block, returning `(cleaned_tail, shielded_dup)` (C-4 (e)). Own blocks before
/// that boundary are removed; everything from the foreign block onward is
/// preserved verbatim — including any own duplicate beyond it, which
/// foreign-safety forbids reaching across; the flag surfaces such a shielded
/// duplicate.
fn canonicalise_tail(tail: &str) -> (String, bool) {
    let foreign = first_real_foreign_block_pos(tail, 0);
    let (head, rest) = (&tail[..foreign], &tail[foreign..]);
    let cleaned = remove_own_blocks(head);
    let shielded = first_own_open_fence_pos(rest).is_some();
    (format!("{cleaned}{rest}"), shielded)
}

/// Locate Loomweave's own block, **foreign-aware**. The start is the first
/// top-level own open fence ([`first_own_open_fence_pos`], so an own marker
/// shielded inside an unclosed foreign block is skipped). A block is
/// [`Span::WellFormed`] only when its own end marker follows the start *and*
/// precedes the first real foreign block; an own close that lies beyond a
/// foreign block is [`Span::Malformed`] (a naive open..close match would swallow
/// the sibling — bounded recovery is required).
fn locate_span(content: &str) -> Span {
    let Some(start) = first_own_open_fence_pos(content) else {
        return Span::Absent;
    };
    let Some((own_end_start, own_end_after)) = own_end_line_pos(content, start) else {
        return Span::Malformed;
    };
    if own_end_start >= first_real_foreign_block_pos(content, start) {
        return Span::Malformed;
    }
    // Body is everything between the start-marker line and the end-marker line;
    // trim a single trailing newline so it round-trips against `canonical_body`.
    let body_start = start + content[start..].find('\n').map_or(0, |i| i + 1);
    let raw_body = &content[body_start..own_end_start];
    let body = raw_body.strip_suffix('\n').unwrap_or(raw_body).to_owned();
    Span::WellFormed {
        end: own_end_after,
        body,
    }
}

// ---------------------------------------------------------------------------
// CLAUDE.md → AGENTS.md redirect routing (C-20)
// ---------------------------------------------------------------------------

/// Is `line` *solely* an @-import of `AGENTS.md`?
///
/// The hand-rolled equivalent of the federation-normative regex
/// `^@(?:\./)?AGENTS\.md$` (case-insensitive), matched against the trimmed line
/// so it is anchored at both ends. Deliberately narrow: `See @AGENTS.md for
/// details` is not solely an import, `@AGENTS.md.bak` names a different file,
/// and `@/AGENTS.md` is an absolute path — treating any of them as a redirect
/// would stop us maintaining the block that `CLAUDE.md` actually carries. The
/// `./` is matched as a unit so only the relative spelling qualifies.
fn is_agents_md_redirect(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix('@') else {
        return false;
    };
    let rest = rest.strip_prefix("./").unwrap_or(rest);
    rest.eq_ignore_ascii_case(AGENTS_MD)
}

/// Blank every managed instruction block, preserving line structure.
///
/// Characters inside any tool's block — Loomweave's own as well as a sibling's —
/// become spaces while newlines survive, so a caller can scan for a redirect
/// line without seeing text a block merely *quotes*. An unclosed block masks to
/// EOF: we cannot prove where it ends, so we decline to read anything past it as
/// free-standing project prose.
///
/// Line-anchored, because [`parse_fence`] is: Loomweave never does a bare `-->`
/// substring scan, which could match a sibling's marker mid-prose. (The peer
/// implementations mask from the fence's byte offset within its line; on a
/// line-anchored fence the two agree, since a fence line holds nothing before
/// its `<!--`.)
fn mask_managed_blocks(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut inside: Option<String> = None;
    for line in content.split_inclusive('\n') {
        let fence = parse_fence(line.trim());
        let masked = match (&inside, &fence) {
            (None, Some((ns, false))) => {
                inside = Some(ns.clone());
                true
            }
            (None, _) => false,
            (Some(open_ns), Some((ns, true))) if open_ns == ns => {
                inside = None;
                true
            }
            (Some(_), _) => true,
        };
        if masked {
            out.extend(line.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Whether `CLAUDE.md` is merely a redirect to `AGENTS.md` (C-20).
///
/// True when, *outside* every managed instruction block, `CLAUDE.md` carries a
/// line that is solely an @-import of `AGENTS.md`. Such a project keeps one
/// source of agent context, so Loomweave maintains its block in `AGENTS.md`
/// alone (see [`instruction_targets`]).
///
/// Anything we cannot read as a plain project file — absent, unreadable, not
/// valid UTF-8, or a symlink — reads as *no* redirect, so the caller keeps the
/// existing dual-write behaviour. Failing safe here costs a redundant block;
/// failing open would silently stop maintaining the block a project actually
/// reads.
///
/// **Known limitation, accepted as spec:** a ```` ``` ````-fenced markdown
/// example of `@AGENTS.md` inside `CLAUDE.md` still triggers. C-20 specifies
/// managed-block exclusion only, and every member implements the *same*
/// limitation on purpose — a file must get the same verdict whichever member's
/// installer runs, and uniformity beats a unilateral widening here.
#[must_use]
pub fn claude_md_redirects_to_agents_md(project_root: &Path) -> bool {
    let path = project_root.join(CLAUDE_MD);
    if is_symlink(&path).unwrap_or(true) {
        return false;
    }
    let Ok(content) = fs::read_to_string(&path) else {
        return false;
    };
    mask_managed_blocks(&content)
        .lines()
        .any(is_agents_md_redirect)
}

/// `(write_to, migrate_off)` agent-context filenames for a project.
///
/// Normally Loomweave dual-writes `CLAUDE.md` and `AGENTS.md` and migrates
/// nothing. When `CLAUDE.md` only redirects to `AGENTS.md` the block belongs in
/// `AGENTS.md` alone, and any legacy block left in `CLAUDE.md` is listed for
/// migration — the redirect already supplies that content, so a second copy is
/// duplicate guidance that drifts.
fn instruction_targets(project_root: &Path) -> (&'static [&'static str], &'static [&'static str]) {
    if claude_md_redirects_to_agents_md(project_root) {
        (&[AGENTS_MD], &[CLAUDE_MD])
    } else {
        (TARGET_FILES, &[])
    }
}

/// Count own open-marker lines in `content`, top-level or not. The split-brain
/// signal for [`remove_instructions`] and [`migrated_file_state`]: counting
/// *every* own marker (including one shielded inside a foreign block) can only
/// make removal more conservative, never less.
fn own_open_marker_count(content: &str) -> usize {
    content
        .lines()
        .filter(|line| line.trim().starts_with(START_PREFIX))
        .count()
}

/// What [`remove_instructions`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoveOutcome {
    /// There was nothing to remove — success, no write.
    Nothing,
    /// The own block was excised (or the file deleted, when it held nothing
    /// else).
    Removed,
    /// Ownership was not provable, so nothing was written. Carries the reason.
    Refused(String),
    /// The target is a symlink; skipped rather than written through.
    SkippedSymlink,
}

/// Remove Loomweave's own instruction block from `path`, or refuse.
///
/// The delete-side mirror of [`install_into_file`], and deliberately **more
/// conservative** than it. Injection can always fall back to an append — which
/// deletes nothing — so it may safely bound a malformed block at a foreign fence
/// and recover. Removal has no such fallback: a mis-bounded delete eats a
/// sibling's block. So every case where ownership is not *provable* is a no-op
/// that writes nothing:
///
/// - no own top-level open fence (including one shielded inside an unclosed
///   foreign block) → nothing to remove;
/// - own close marker missing, or sitting beyond a real foreign block → refuse;
/// - more than one own block (split brain) → refuse, matching doctor's "resolve
///   it by hand" posture rather than guessing which copy to drop.
///
/// Own-namespace-only and foreign-safe throughout (C-4): the bounds come from
/// the same [`first_own_open_fence_pos`] / [`first_real_foreign_block_pos`] pair
/// the injector uses, so a Loomweave marker quoted inside a sibling's block is
/// never mistaken for ours.
///
/// # Errors
///
/// Returns an error only if the stat, temp write, rename, or unlink fails; an
/// unreadable file is reported as a refusal, not an error.
fn remove_instructions(path: &Path) -> Result<RemoveOutcome> {
    if is_symlink(path).with_context(|| format!("stat {}", path.display()))? {
        return Ok(RemoveOutcome::SkippedSymlink);
    }

    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RemoveOutcome::Nothing);
        }
        Err(err) => {
            return Ok(RemoveOutcome::Refused(format!(
                "could not read {}: {err}",
                path.display()
            )));
        }
    };

    let Some(start) = first_own_open_fence_pos(&content) else {
        return Ok(RemoveOutcome::Nothing);
    };

    let blocks = own_open_marker_count(&content);
    if blocks > 1 {
        return Ok(RemoveOutcome::Refused(format!(
            "{} carries {blocks} Loomweave instruction blocks (split brain); refusing to \
             guess which copy to remove — resolve it by hand",
            path.display()
        )));
    }

    let Some((own_end_start, own_end_after)) = own_end_line_pos(&content, start) else {
        return Ok(RemoveOutcome::Refused(unclosed_reason(path)));
    };
    if own_end_start >= first_real_foreign_block_pos(&content, start) {
        return Ok(RemoveOutcome::Refused(unclosed_reason(path)));
    }

    // Excise, then tidy only at the seam. `append_block` separates with a blank
    // line, so a naive cut leaves a doubled one; trimming newlines on each side
    // of the seam restores the single blank line without touching blank runs
    // elsewhere in the file.
    let head = content[..start].trim_end_matches('\n');
    let tail = content[own_end_after..].trim_start_matches('\n');
    let remainder = match (head.is_empty(), tail.is_empty()) {
        (false, false) => format!("{head}\n\n{tail}"),
        (true, _) => tail.to_owned(),
        (_, true) => head.to_owned(),
    };

    if remainder.trim().is_empty() {
        // The file held nothing but Loomweave's block — i.e. exactly what the
        // installer creates for a missing file. Removing it is the symmetric
        // inverse of that create; leaving a blank file behind would also trip
        // `atomic_write`'s refuse-to-empty guard (C-4 (g)).
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
        return Ok(RemoveOutcome::Removed);
    }

    let remainder = if remainder.ends_with('\n') {
        remainder
    } else {
        format!("{remainder}\n")
    };
    atomic_write(path, &remainder)?;
    Ok(RemoveOutcome::Removed)
}

fn unclosed_reason(path: &Path) -> String {
    format!(
        "the Loomweave instruction block in {} is unclosed or its close marker sits beyond \
         another tool's block; refusing to delete across a boundary we cannot prove is ours",
        path.display()
    )
}

/// Outcome of an [`install_instructions`] call.
#[derive(Debug, Clone)]
pub struct InstructionsInstallReport {
    /// True if any target file's bytes were (re)written this call; false if
    /// every file already held the current well-formed block.
    pub changed: bool,
    /// Target paths skipped because they are symlinks (Loomweave never writes
    /// through a symlink — temp+rename would silently convert the link into a
    /// regular file). Each skip is also surfaced as a stderr warning; callers
    /// that gate on convergence (doctor `--fix`) use this to name the hand
    /// remedy instead of reporting an opaque non-convergence.
    pub skipped_symlinks: Vec<PathBuf>,
}

/// Inject (or repair) the Loomweave block into a project's agent-context files
/// under `project_root`, idempotently. Doubles as the `doctor --fix` repair.
///
/// **Redirect routing (C-20).** The target set comes from
/// [`instruction_targets`]: normally both [`TARGET_FILES`], but when `CLAUDE.md`
/// is merely a redirect to `AGENTS.md` the block belongs in `AGENTS.md` alone
/// and a legacy `CLAUDE.md` block is migrated off. Migration runs **first**, so
/// a redirecting project never briefly carries two blocks. `AGENTS.md` is
/// created if absent, exactly as for a first install.
///
/// The removal side is deliberately more conservative than this one — see
/// [`remove_instructions`]. A refusal is warned about and left in place; it
/// never fails the install, and it never writes.
///
/// Per-file behaviour, touching **only** Loomweave's own span and obeying the
/// weft C-4 multi-owner managed-block contract — a rewrite never crosses a
/// co-resident foreign-namespace fence:
///
/// - **Replace** when a well-ordered own block closes *before* any real foreign
///   block: rewrite that span in place, then canonicalise any duplicate own
///   blocks in the tail up to (never across) the first real foreign block
///   (C-4 (c) replace path, (e) own-duplicate canonicalisation). A no-op when
///   the result is byte-identical.
/// - **Bounded recovery** when the own close lies *beyond* a real foreign block
///   (so a naive open..close match would swallow the sibling): cut the rewrite
///   at the foreign fence, never truncate across it (C-4 (c)).
/// - **Append** when no claimable own start is present — none at all, or one
///   shielded inside an unclosed foreign block: append the block to the file's
///   existing content, which is left intact (C-4 (d)).
/// - **Dangling start marker** (own start present, no following own end): strip
///   the orphaned start-marker line(s) and append a fresh block; all other bytes
///   survive. Never truncate to EOF.
///
/// Writes are atomic (temp + rename in the same directory, preserving the
/// existing file mode) and refuse an empty payload (C-4 (g)).
///
/// A **symlinked target is skipped, not fatal**: writing through it would
/// silently convert the link into a regular file, but some projects ship one of
/// the targets as a symlink (rust-analyzer's `AGENTS.md` points into `docs/`),
/// and aborting would take the rest of the install down with it. The skip is
/// surfaced as a stderr warning naming the file and the by-hand remedy, and
/// recorded in [`InstructionsInstallReport::skipped_symlinks`]; the other
/// target is still processed.
///
/// # Errors
///
/// Returns an error if any read, stat, temp write, or rename fails.
pub fn install_instructions(project_root: &Path) -> Result<InstructionsInstallReport> {
    let mut changed = false;
    let mut skipped_symlinks = Vec::new();
    let (write_to, migrate_off) = instruction_targets(project_root);

    for name in migrate_off {
        let path = project_root.join(name);
        let outcome = remove_instructions(&path).with_context(|| {
            format!(
                "migrate loomweave instructions out of {} (it redirects to {AGENTS_MD})",
                path.display()
            )
        })?;
        match outcome {
            RemoveOutcome::Removed => changed = true,
            RemoveOutcome::Nothing => {}
            // Degrade, don't abort — same posture as the symlink skip below. A
            // legacy block we cannot prove is ours stays put; the redirect still
            // delivers the current copy from AGENTS.md, so the failure mode is a
            // stale duplicate, never missing guidance. To stderr, never stdout
            // (operator-diagnostic hygiene).
            RemoveOutcome::Refused(reason) => eprintln!("loomweave: warning: {reason}"),
            RemoveOutcome::SkippedSymlink => {
                eprintln!(
                    "loomweave: warning: skipping {}: it is a symlink (loomweave never writes \
                     through a symlink; replace the link with a regular file by hand, then \
                     re-run `loomweave install` or `loomweave doctor --fix`)",
                    path.display()
                );
                skipped_symlinks.push(path);
            }
        }
    }

    for name in write_to {
        let path = project_root.join(name);
        if is_symlink(&path)
            .with_context(|| format!("inject loomweave instructions into {}", path.display()))?
        {
            // Degrade, don't abort: one symlinked orientation file must not
            // sink the store/db/plugin wiring or the sibling target. To
            // stderr, never stdout (operator-diagnostic hygiene).
            eprintln!(
                "loomweave: warning: skipping {}: it is a symlink (loomweave never writes \
                 through a symlink; replace the link with a regular file by hand, then re-run \
                 `loomweave install` or `loomweave doctor --fix`)",
                path.display()
            );
            skipped_symlinks.push(path);
            continue;
        }
        changed |= install_into_file(&path)
            .with_context(|| format!("inject loomweave instructions into {}", path.display()))?;
    }
    Ok(InstructionsInstallReport {
        changed,
        skipped_symlinks,
    })
}

fn install_into_file(path: &Path) -> Result<bool> {
    reject_symlink(path)?;

    let existing = match fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| format!("read {}", path.display()));
        }
    };

    let block = render_block();
    let (new_content, shielded_dup) = match existing.as_deref() {
        None => (format!("{block}\n"), false),
        Some(content) => compute_injection(content, &block),
    };

    if shielded_dup {
        // A stale own duplicate survives beyond a foreign block because
        // canonicalising it would mean reaching across a block we do not own. It
        // is conflicting guidance, not a harmless copy — surface it rather than
        // silently ship a split brain (foreign-safety wins over own-dedup, C-4
        // (e)). To stderr, never stdout (operator-diagnostic hygiene).
        eprintln!(
            "loomweave: warning: {} carries a Loomweave instruction block beyond \
             another tool's block that cannot be canonicalised without crossing it; \
             the stale copy was left in place. Resolve it by hand.",
            path.display()
        );
    }

    if existing.as_deref() == Some(new_content.as_str()) {
        return Ok(false);
    }
    atomic_write(path, &new_content)?;
    Ok(true)
}

/// Compute the new file content for an existing file, foreign-fence-bounded.
/// Returns `(new_content, shielded_dup)`; `shielded_dup` flags an own duplicate
/// left in place because canonicalising it would cross a foreign block. Pure —
/// all IO is in [`install_into_file`].
fn compute_injection(content: &str, block: &str) -> (String, bool) {
    let Some(start) = first_own_open_fence_pos(content) else {
        // No own open we can claim (none, or shielded inside an unclosed foreign
        // block). Append a fresh block, preserving all existing text (C-4 (d)).
        // If a current block is already present-but-unreachable, appending each
        // run would grow the file unboundedly — decline instead (read-only, so
        // foreign-safety is untouched).
        if content.contains(block) {
            return (content.to_owned(), false);
        }
        return (append_block(content, block), false);
    };

    let Some((own_end_start, own_end_after)) = own_end_line_pos(content, start) else {
        // Own open with no following end marker anywhere: dangling. Strip every
        // orphan own start line (convergent + foreign-safe) and append a fresh
        // block. Never truncate to EOF (C-4 (d)).
        let stripped = strip_start_marker_line(content);
        return (append_block(&stripped, block), false);
    };

    let foreign = first_real_foreign_block_pos(content, start);
    if own_end_start < foreign {
        // Well-formed own block closing before any real foreign block: replace it
        // in place, then canonicalise duplicate own blocks in the tail up to (but
        // never across) the first real foreign block (C-4 (c) replace path, (e)).
        let (tail, shielded) = canonicalise_tail(&content[own_end_after..]);
        (splice_block(content, start, block, &tail), shielded)
    } else {
        // Bounded recovery (C-4 (c)): the own close lies beyond a real foreign
        // block, so a naive open..close match would swallow the sibling. Cut at
        // the foreign block (or EOF) instead, never across it.
        let tail = &content[foreign..];
        let shielded = first_own_open_fence_pos(tail).is_some();
        (splice_block(content, start, block, tail), shielded)
    }
}

/// Splice `block` in at `start`, followed by `tail`, with exactly one newline
/// between the block's end marker and the tail. When the in-place span is
/// unchanged this reproduces `content` byte-for-byte (idempotency); when `tail`
/// begins at a foreign fence it guarantees our close marker is never glued
/// mid-line against that fence.
fn splice_block(content: &str, start: usize, block: &str, tail: &str) -> String {
    format!("{}{}\n{}", &content[..start], block, tail)
}

/// Append `block` to `content`, separated by a blank line, with a trailing
/// newline. `content`'s existing bytes are preserved verbatim.
fn append_block(content: &str, block: &str) -> String {
    if content.is_empty() {
        return format!("{block}\n");
    }
    let sep = if content.ends_with("\n\n") {
        ""
    } else if content.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{content}{sep}{block}\n")
}

/// Remove **every** line whose trimmed form starts with [`START_PREFIX`].
/// Every other byte — including any orphaned body that followed it — is kept.
///
/// This is only reached from [`compute_injection`]'s dangling-own-open branch,
/// where [`own_end_line_pos`] found no end marker following the first start
/// marker — so *every* start marker in the file is orphaned by definition.
/// Stripping only the first would leave a second dangling start behind; on the
/// next install/doctor run that leftover orphan would pair with the
/// freshly-appended block's end marker, forming a well-formed span that engulfs
/// (and deletes) everything between — including a co-resident Filigree block.
/// Removing all orphan starts converges in one pass and never eats a
/// neighbouring tool's block.
fn strip_start_marker_line(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        if line.trim().starts_with(START_PREFIX) {
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Is `path` a symlink? A non-existent path is not (we create it). Used by
/// [`install_instructions`] to *skip* a symlinked target with a warning;
/// [`reject_symlink`] stays in the write path as belt-and-braces.
fn is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

/// Reject a symlinked target so temp+rename never silently converts a link into
/// a regular file. A non-existent path is fine (we create it).
fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "refusing to write {}: it is a symlink (resolve it by hand, then re-run)",
                path.display()
            );
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

/// Atomically write `content` to `path`: stage into a sibling temp file in the
/// same directory (so `rename` stays on one filesystem), preserve the existing
/// file mode when the target already exists, then `rename` over the target.
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    // Refuse-to-empty guard (C-4 (g)). Every caller embeds the non-empty rendered
    // block, so an empty / whitespace-only payload can only be corruption or a
    // logic bug. The temp+rename below already makes truncating a populated file
    // structurally impossible; this is belt-and-braces parity with the siblings —
    // refuse loudly rather than rename an empty temp over a populated CLAUDE.md /
    // AGENTS.md.
    if content.trim().is_empty() {
        bail!("refusing to write empty content to {}", path.display());
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;

    let file_name = path.file_name().map_or_else(
        || "instructions".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let temp_path: PathBuf = parent.join(format!(
        ".{}.loomweave.tmp-{}",
        file_name,
        std::process::id()
    ));

    // Cleanup guard: drop the staged temp file if any step after creating it
    // fails, so a failed write never leaks a `.tmp-*` sibling.
    if let Err(err) = write_temp_then_rename(&temp_path, path, content) {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

fn write_temp_then_rename(temp_path: &Path, path: &Path, content: &str) -> Result<()> {
    fs::write(temp_path, content).with_context(|| format!("write {}", temp_path.display()))?;
    #[cfg(unix)]
    preserve_mode(path, temp_path)?;
    fs::rename(temp_path, path)
        .with_context(|| format!("rename {} -> {}", temp_path.display(), path.display()))?;
    Ok(())
}

/// Copy the existing file's permission bits onto the staged temp file so the
/// rename preserves mode. A no-op when the target does not yet exist.
#[cfg(unix)]
fn preserve_mode(path: &Path, temp_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = fs::metadata(path) else {
        return Ok(());
    };
    let mode = meta.permissions().mode();
    fs::set_permissions(temp_path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("preserve mode on {}", temp_path.display()))
}

#[cfg(test)]
mod tests {
    use super::{
        END_MARKER, INSTRUCTIONS_BODY, InstructionsState, START_PREFIX, TARGET_FILES,
        canonical_body, install_instructions, instructions_state, render_block,
    };

    /// A representative Filigree two-block neighbour, taken verbatim in shape
    /// from this repo's own `AGENTS.md`. The coexistence tests assert these
    /// bytes survive every operation untouched.
    const FILIGREE_BLOCK: &str = "<!-- filigree:instructions:v3.0.0rc2:98d5c5f2 -->\n\
## Filigree Issue Tracker\n\
\n\
filigree tracks tasks for this project.\n\
<!-- /filigree:instructions -->\n";

    #[test]
    fn asset_is_thin_and_pointer_shaped() {
        // The plan caps the always-loaded body at ~15-25 lines: a pointer, not
        // a manual. Guard against it growing into a second skill.
        let lines = INSTRUCTIONS_BODY.lines().count();
        assert!(
            lines <= 30,
            "instructions body grew to {lines} lines; keep it thin (a pointer)"
        );
        assert!(INSTRUCTIONS_BODY.contains("mcp__loomweave__"));
        assert!(INSTRUCTIONS_BODY.contains("loomweave-workflow"));
    }

    /// C-20 budgets the always-loaded surface by number, not by taste: the
    /// block Loomweave writes into a consuming project's `CLAUDE.md` /
    /// `AGENTS.md` is paid for on every session, before the first tool call.
    ///
    /// Measured on the **rendered block** — markers included — because that is
    /// what actually lands in the file; the bare asset is asserted too so both
    /// readings of the convention hold. A version bump lengthens the start
    /// marker, so the headroom here is deliberate, not slack to spend.
    #[test]
    fn rendered_block_fits_the_c20_budget() {
        let asset = INSTRUCTIONS_BODY.trim_end().chars().count();
        assert!(
            asset <= 800,
            "instructions asset is {asset} chars (C-20 budget 800)"
        );
        let block = render_block().chars().count();
        assert!(
            block <= 800,
            "rendered instructions block is {block} chars (C-20 budget 800) — it is a \
             pointer: what Loomweave is, when to reach for it, where the full reference \
             lives, and at most two load-bearing rules. Depth belongs in the \
             loomweave-workflow skill."
        );
    }

    #[test]
    fn start_prefix_is_not_a_prefix_of_end_marker() {
        // Detection keys the start on START_PREFIX and the end on an exact
        // END_MARKER line; the `/` keeps the end marker from matching the start
        // prefix. Pin that invariant.
        assert!(!END_MARKER.starts_with(START_PREFIX));
    }

    #[test]
    fn render_round_trips_to_canonical_body() {
        let block = render_block();
        assert!(block.starts_with(START_PREFIX));
        assert!(block.contains("<!-- loomweave:last-writer:loomweave install -->"));
        assert!(block.ends_with(END_MARKER));
        // Wrapping the rendered block in a file and re-extracting must yield the
        // canonical body, or idempotency breaks (install -> Drifted -> "fix"
        // every run).
        let file = format!("prefix\n\n{block}\n");
        let state = super::locate_span(&file);
        match state {
            super::Span::WellFormed { body, .. } => assert_eq!(body, canonical_body()),
            _ => panic!("rendered block did not locate as well-formed"),
        }
    }

    #[test]
    fn create_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let report = install_instructions(dir.path()).unwrap();
        assert!(report.changed, "first install should write");
        for name in ["CLAUDE.md", "AGENTS.md"] {
            let body = std::fs::read_to_string(dir.path().join(name)).unwrap();
            assert!(body.starts_with(START_PREFIX), "{name} missing block");
            assert!(body.trim_end().ends_with(END_MARKER));
        }
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn install_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(install_instructions(dir.path()).unwrap().changed);
        let second = install_instructions(dir.path()).unwrap();
        assert!(
            !second.changed,
            "second install must be a no-op on byte-identical body"
        );
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn append_preserves_prior_content() {
        let dir = tempfile::tempdir().unwrap();
        let prior = "# Project notes\n\nSome existing prose.\n";
        for name in ["CLAUDE.md", "AGENTS.md"] {
            std::fs::write(dir.path().join(name), prior).unwrap();
        }
        assert!(install_instructions(dir.path()).unwrap().changed);
        for name in ["CLAUDE.md", "AGENTS.md"] {
            let body = std::fs::read_to_string(dir.path().join(name)).unwrap();
            assert!(body.starts_with(prior), "prior content not preserved");
            assert!(body.contains(START_PREFIX));
        }
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn replace_rewrites_on_drift_only() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        // Hand-edit the body inside the Loomweave span on one file.
        let claude = dir.path().join("CLAUDE.md");
        let content = std::fs::read_to_string(&claude).unwrap();
        let drifted = content.replace("## Loomweave", "## DRIFTED HEADER");
        assert_ne!(drifted, content, "test setup: substitution must apply");
        std::fs::write(&claude, &drifted).unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::Drifted);

        let report = install_instructions(dir.path()).unwrap();
        assert!(report.changed, "drift must trigger a rewrite");
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn state_missing_before_install() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::Missing);
    }

    #[test]
    fn state_missing_when_one_file_lacks_block() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        // Remove the block from AGENTS.md entirely.
        std::fs::write(dir.path().join("AGENTS.md"), "# just notes\n").unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::Missing);
    }

    /// The headline coexistence guarantee: a file pre-seeded with a Filigree
    /// block survives create / append / replace / malformed round-trips with
    /// Filigree's bytes untouched.
    #[test]
    fn filigree_block_survives_every_operation() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("CLAUDE.md");
        let agents = dir.path().join("AGENTS.md");

        // Seed both files with only the Filigree block (the append/create case).
        std::fs::write(&claude, FILIGREE_BLOCK).unwrap();
        std::fs::write(&agents, FILIGREE_BLOCK).unwrap();

        // 1. Append: Loomweave block added, Filigree bytes intact.
        install_instructions(dir.path()).unwrap();
        for path in [&claude, &agents] {
            let body = std::fs::read_to_string(path).unwrap();
            assert!(
                body.contains(FILIGREE_BLOCK),
                "filigree block lost on append"
            );
            assert!(body.contains(START_PREFIX), "loomweave block missing");
        }
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);

        // 2. Replace (drift): edit the Loomweave body; Filigree still survives.
        let content = std::fs::read_to_string(&claude).unwrap();
        let drifted = content.replace("## Loomweave", "## EDITED");
        assert_ne!(drifted, content, "test setup: substitution must apply");
        std::fs::write(&claude, &drifted).unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::Drifted);
        install_instructions(dir.path()).unwrap();
        let repaired = std::fs::read_to_string(&claude).unwrap();
        assert!(
            repaired.contains(FILIGREE_BLOCK),
            "filigree block lost on drift repair"
        );
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);

        // 3. Malformed (dangling Loomweave start marker, with the Filigree block
        //    present): repair must NOT truncate to EOF and eat Filigree.
        let dangling = format!(
            "{FILIGREE_BLOCK}\n<!-- loomweave:instructions:v0:deadbeef -->\nstale orphan body\n"
        );
        std::fs::write(&agents, &dangling).unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::Malformed);
        install_instructions(dir.path()).unwrap();
        let fixed = std::fs::read_to_string(&agents).unwrap();
        assert!(
            fixed.contains(FILIGREE_BLOCK),
            "filigree block eaten by dangling-marker repair"
        );
        assert!(
            fixed.contains("stale orphan body"),
            "orphaned body should be left as loose prose, not deleted"
        );
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    /// Regression: two dangling Loomweave start markers (no intervening end
    /// marker) co-resident with a Filigree block. The Malformed-branch repair
    /// must strip BOTH orphan starts, not just the first — otherwise the leftover
    /// orphan re-pairs with the freshly-appended block's end marker on a later
    /// run, forming a well-formed span that engulfs and deletes the Filigree
    /// block (silent data loss) and never converges. Asserts (a) Filigree bytes
    /// survive and (b) the repair reaches a fixed point in a single pass.
    #[test]
    fn two_dangling_starts_with_filigree_block_converge_in_one_pass() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("AGENTS.md");
        // Give the other target file a clean block so AGENTS.md is the only
        // malformed file driving the aggregate state.
        install_instructions(dir.path()).unwrap();

        // AGENTS.md: TWO dangling loomweave start markers (no end marker
        // between them) sitting BEFORE the Filigree block (bad copy-paste /
        // merge artifact). The ordering is load-bearing: it puts the leftover
        // orphan start on the near side of the Filigree block, so the buggy
        // strip-first path leaves an orphan that — on the next run — pairs with
        // the appended block's end marker and engulfs (deletes) the Filigree
        // bytes. Assertion (a) below then fails on the unfixed code, exercising
        // the literal data-loss mechanism, not merely non-convergence.
        let doubled = format!(
            "<!-- loomweave:instructions:v0:deadbeef -->\n\
             first orphan body\n\
             <!-- loomweave:instructions:v0:cafef00d -->\n\
             second orphan body\n\
             \n\
             {FILIGREE_BLOCK}"
        );
        std::fs::write(&agents, &doubled).unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::Malformed);

        // (a) Drive repeated install passes — the way `doctor --fix` runs over
        // a project's lifetime. The data-loss mechanism only fires on the SECOND
        // pass: the buggy strip-first repair leaves an orphan start that
        // `locate_span` then pairs with pass-1's appended end marker, forming a
        // well-formed span that engulfs the Filigree block, so pass 2's splice
        // deletes it. Assert the Filigree bytes survive after EVERY pass, so the
        // literal deletion is the load-bearing failure on the unfixed code.
        for pass in 1..=3 {
            install_instructions(dir.path()).unwrap();
            let after = std::fs::read_to_string(&agents).unwrap();
            assert!(
                after.contains(FILIGREE_BLOCK),
                "filigree block eaten by two-dangling-start repair on pass {pass}"
            );
        }

        // (b) The repair reaches a fixed point: a single pass from Malformed must
        // converge to UpToDate (not "repair did not converge"), and further
        // passes are no-ops.
        std::fs::write(&agents, &doubled).unwrap();
        install_instructions(dir.path()).unwrap();
        assert_eq!(
            instructions_state(dir.path()),
            InstructionsState::UpToDate,
            "two-dangling-start repair must reach a fixed point in a single pass"
        );
        let second = install_instructions(dir.path()).unwrap();
        assert!(
            !second.changed,
            "repaired file must be a stable fixed point (no further rewrite)"
        );

        let fixed = std::fs::read_to_string(&agents).unwrap();
        assert!(
            fixed.contains(FILIGREE_BLOCK),
            "filigree block must survive the converged repair"
        );
        // Both orphaned bodies survive as loose prose; no bytes outside our span lost.
        assert!(fixed.contains("first orphan body"));
        assert!(fixed.contains("second orphan body"));
        // Exactly one well-formed start marker remains (the appended block).
        assert_eq!(
            fixed.matches(START_PREFIX).count(),
            1,
            "exactly one start marker must remain after stripping both orphans"
        );
    }

    #[test]
    fn dangling_start_marker_is_malformed_then_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("CLAUDE.md");
        let agents = dir.path().join("AGENTS.md");
        // One file gets a clean block so only the dangling file is malformed.
        install_instructions(dir.path()).unwrap();
        std::fs::write(
            &claude,
            "# notes\n<!-- loomweave:instructions:v0:deadbeef -->\norphan body, no end marker\n",
        )
        .unwrap();
        let _ = &agents;
        assert_eq!(instructions_state(dir.path()), InstructionsState::Malformed);

        install_instructions(dir.path()).unwrap();
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
        let fixed = std::fs::read_to_string(&claude).unwrap();
        assert!(fixed.contains("# notes"), "leading content eaten");
        assert!(
            fixed.contains("orphan body, no end marker"),
            "orphan body should survive as loose prose"
        );
        // Exactly one well-formed start marker remains.
        assert_eq!(fixed.matches(START_PREFIX).count(), 1);
    }

    /// A symlinked target is never written through (that part of the contract
    /// is unchanged), but it degrades to a per-file *skip* rather than aborting
    /// the whole call: the regular-file sibling is still injected and the
    /// skipped path is reported (Sprint-3 QA, rust-analyzer's symlinked
    /// AGENTS.md aborting `loomweave install`).
    #[cfg(unix)]
    #[test]
    fn symlink_target_is_skipped_not_fatal() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.md");
        std::fs::write(&real, "real contents\n").unwrap();
        symlink(&real, dir.path().join("CLAUDE.md")).unwrap();

        let report = install_instructions(dir.path()).unwrap();
        assert!(report.changed, "AGENTS.md must still be injected");
        assert_eq!(
            report.skipped_symlinks,
            vec![dir.path().join("CLAUDE.md")],
            "the symlinked target must be reported as skipped"
        );

        // Symlink + target untouched; sibling injected.
        assert!(
            std::fs::symlink_metadata(dir.path().join("CLAUDE.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "real contents\n");
        assert!(
            std::fs::read_to_string(dir.path().join("AGENTS.md"))
                .unwrap()
                .contains(START_PREFIX)
        );
    }

    /// A representative *uppercase*-namespaced sibling block. C-4 (h) requires
    /// the foreign-fence detector to match the namespace case-insensitively, so
    /// this must register as a boundary exactly like a lowercase sibling.
    const FILIGREE_BLOCK_UPPER: &str = "<!-- FILIGREE:instructions:v3.0.0rc2:98d5c5f2 -->\n\
## Filigree Issue Tracker\n\
\n\
filigree tracks tasks for this project.\n\
<!-- /FILIGREE:instructions -->\n";

    /// C-4 (c) — the headline data-loss vector. A stale Loomweave START marker,
    /// then a *complete* Filigree block, then Loomweave's REAL end marker. The
    /// pre-fix `locate_span` closes the own span on the first end-marker line
    /// after the start — Loomweave's own close, which sits AFTER the Filigree
    /// block — so the blind `splice_span` deletes the sibling (1 → 0). The fix
    /// must bound the replaced span at the first foreign fence after the own
    /// start. Demonstrably fails on the pre-change code.
    #[test]
    fn foreign_block_sandwiched_in_own_span_survives_replace() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap(); // seed both files clean
        let claude = dir.path().join("CLAUDE.md");
        let sandwiched = format!(
            "# notes\n\n\
             <!-- loomweave:instructions:v0:deadbeef -->\n\
             stale loomweave body\n\
             {FILIGREE_BLOCK}\
             <!-- /loomweave:instructions -->\n\
             trailing prose\n"
        );
        std::fs::write(&claude, &sandwiched).unwrap();

        install_instructions(dir.path()).unwrap();
        let after = std::fs::read_to_string(&claude).unwrap();
        assert!(
            after.contains(FILIGREE_BLOCK),
            "filigree block swallowed by unbounded own-span replace:\n{after}"
        );
        assert_eq!(
            after.matches(START_PREFIX).count(),
            1,
            "exactly one loomweave start must remain:\n{after}"
        );
        // Foreign-untouched bytes plus convergence: a second pass is a no-op.
        let second = install_instructions(dir.path()).unwrap();
        assert!(
            !second.changed,
            "sandwiched-foreign repair must reach a fixed point:\n{}",
            std::fs::read_to_string(&claude).unwrap()
        );
        assert!(
            std::fs::read_to_string(&claude)
                .unwrap()
                .contains(FILIGREE_BLOCK),
            "filigree block must survive the converged repair"
        );
    }

    /// C-4 (h) — same sandwich as the headline vector, but the sibling uses an
    /// UPPERCASE namespace. The case-insensitive fence detector must still treat
    /// it as a foreign boundary and refuse to swallow it.
    #[test]
    fn uppercase_foreign_namespace_registers_as_boundary() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        let claude = dir.path().join("CLAUDE.md");
        let sandwiched = format!(
            "<!-- loomweave:instructions:v0:deadbeef -->\n\
             stale\n\
             {FILIGREE_BLOCK_UPPER}\
             <!-- /loomweave:instructions -->\n"
        );
        std::fs::write(&claude, &sandwiched).unwrap();
        install_instructions(dir.path()).unwrap();
        let after = std::fs::read_to_string(&claude).unwrap();
        assert!(
            after.contains(FILIGREE_BLOCK_UPPER),
            "uppercase-namespaced foreign block swallowed:\n{after}"
        );
    }

    /// C-4 (e) — own-duplicate canonicalisation. Two well-formed, body-current
    /// Loomweave blocks. Pre-fix: only the first is touched, the stale duplicate
    /// persists, and `instructions_state` still reports `UpToDate` (doctor green
    /// over a split brain). The fix must collapse to exactly one block and make
    /// doctor flag the duplicate before repair. Demonstrably fails on pre-change
    /// code (stays at two; reports `UpToDate`).
    #[test]
    fn duplicate_own_blocks_canonicalise_to_one() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        let claude = dir.path().join("CLAUDE.md");
        let block = render_block();
        let doubled = format!("# notes\n\n{block}\n\n{block}\n");
        std::fs::write(&claude, &doubled).unwrap();

        // doctor must FLAG it (not report green) before repair.
        assert_ne!(
            instructions_state(dir.path()),
            InstructionsState::UpToDate,
            "a stale own-duplicate must not read as healthy"
        );

        // --fix (== install) must collapse to exactly one canonical block.
        install_instructions(dir.path()).unwrap();
        let after = std::fs::read_to_string(&claude).unwrap();
        assert_eq!(
            after.matches(START_PREFIX).count(),
            1,
            "duplicate own block not collapsed:\n{after}"
        );
        assert_eq!(
            after.matches(END_MARKER).count(),
            1,
            "duplicate end marker not collapsed:\n{after}"
        );
        assert!(after.contains("# notes"), "prior prose lost:\n{after}");
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
        // Idempotent thereafter.
        assert!(!install_instructions(dir.path()).unwrap().changed);
    }

    /// C-4 (e) foreign-safety clause: an own duplicate that sits *beyond* a
    /// foreign block must be left in place (canonicalising it would mean reaching
    /// across a block we do not own) and surfaced, never relocated or deleted.
    #[test]
    fn own_duplicate_beyond_foreign_is_left_in_place() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        let claude = dir.path().join("CLAUDE.md");
        let block = render_block();
        let layout = format!("{block}\n\n{FILIGREE_BLOCK}\n{block}\n");
        std::fs::write(&claude, &layout).unwrap();
        install_instructions(dir.path()).unwrap();
        let after = std::fs::read_to_string(&claude).unwrap();
        assert!(
            after.contains(FILIGREE_BLOCK),
            "foreign block lost:\n{after}"
        );
        assert_eq!(
            after.matches(START_PREFIX).count(),
            2,
            "an own duplicate beyond a foreign block must be left, not relocated:\n{after}"
        );
    }

    /// C-4 (g) — refuse-to-empty guard. The atomic writer must refuse an
    /// empty/whitespace-only payload and leave the populated target untouched.
    /// Demonstrably fails on pre-change code (no explicit guard).
    #[test]
    fn atomic_write_refuses_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "populated content\n").unwrap();
        let err = super::atomic_write(&path, "   \n\t\n").unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("empty")
                || err
                    .chain()
                    .any(|c| c.to_string().to_lowercase().contains("empty")),
            "expected a refuse-to-empty error, got: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "populated content\n",
            "populated target must be untouched by a refused empty write"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("CLAUDE.md");
        std::fs::write(&claude, "# notes\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o640)).unwrap();
        install_instructions(dir.path()).unwrap();
        let mode = std::fs::metadata(&claude).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "file mode not preserved across rewrite");
    }

    // -----------------------------------------------------------------------
    // CLAUDE.md -> AGENTS.md redirect routing (C-20)
    // -----------------------------------------------------------------------

    use super::{RemoveOutcome, claude_md_redirects_to_agents_md, remove_instructions};

    /// The real exemplar's shape: a redirect line plus ordinary project prose.
    const REDIRECT_CLAUDE_MD: &str = "# Project\n\n@AGENTS.md\n";

    fn write(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn read(dir: &std::path::Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    #[test]
    fn redirect_line_matches_only_a_bare_agents_import() {
        for yes in [
            "@AGENTS.md",
            "@./AGENTS.md",
            "  @AGENTS.md  ",
            "@agents.md",
            "@AGENTS.MD",
        ] {
            assert!(super::is_agents_md_redirect(yes), "should match: {yes:?}");
        }
        for no in [
            "See @AGENTS.md for details",
            "@AGENTS.md.bak",
            "@/AGENTS.md",
            "@AGENTS",
            "AGENTS.md",
            "@../AGENTS.md",
            "",
        ] {
            assert!(
                !super::is_agents_md_redirect(no),
                "should not match: {no:?}"
            );
        }
    }

    #[test]
    fn redirect_routes_the_block_to_agents_md_only() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", REDIRECT_CLAUDE_MD);

        assert!(claude_md_redirects_to_agents_md(dir.path()));
        assert!(install_instructions(dir.path()).unwrap().changed);

        // AGENTS.md was absent: the installer creates it and writes the block.
        let agents = read(dir.path(), "AGENTS.md");
        assert!(
            agents.contains(START_PREFIX),
            "block missing from AGENTS.md"
        );
        assert_eq!(
            read(dir.path(), "CLAUDE.md"),
            REDIRECT_CLAUDE_MD,
            "the redirecting CLAUDE.md must be left byte-identical"
        );
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn redirect_migrates_a_legacy_claude_md_block_out() {
        let dir = tempfile::tempdir().unwrap();
        // Seed the pre-redirect dual-write state, then add the redirect line.
        install_instructions(dir.path()).unwrap();
        let seeded = read(dir.path(), "CLAUDE.md");
        write(
            dir.path(),
            "CLAUDE.md",
            &format!("# Project\n\n@AGENTS.md\n\n{seeded}"),
        );

        assert_eq!(
            instructions_state(dir.path()),
            InstructionsState::RedirectStale,
            "a leftover block under a redirect is a gate-failing defect, not health"
        );

        assert!(install_instructions(dir.path()).unwrap().changed);
        let claude = read(dir.path(), "CLAUDE.md");
        assert!(
            !claude.contains(START_PREFIX),
            "legacy block not migrated out of CLAUDE.md:\n{claude}"
        );
        assert!(
            claude.contains("@AGENTS.md"),
            "the redirect line must survive migration — eat it and the file stops \
             redirecting, so the next run re-injects (an infinite migrate/re-inject loop)"
        );
        assert!(read(dir.path(), "AGENTS.md").contains(START_PREFIX));
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn migration_preserves_a_coresident_foreign_block() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        let seeded = read(dir.path(), "CLAUDE.md");
        write(
            dir.path(),
            "CLAUDE.md",
            &format!("# Project\n\n@AGENTS.md\n\n{seeded}\n{FILIGREE_BLOCK}"),
        );

        install_instructions(dir.path()).unwrap();
        let claude = read(dir.path(), "CLAUDE.md");
        assert!(!claude.contains(START_PREFIX), "own block not migrated");
        assert!(
            claude.contains(FILIGREE_BLOCK),
            "migration ate the co-resident filigree block:\n{claude}"
        );
        assert!(claude.contains("@AGENTS.md"), "redirect line lost");
    }

    #[test]
    fn agents_md_quoted_inside_a_foreign_block_does_not_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let quoted = "# Project\n\n<!-- filigree:instructions:v3:abc -->\n@AGENTS.md\n\
                      <!-- /filigree:instructions -->\n";
        write(dir.path(), "CLAUDE.md", quoted);

        assert!(
            !claude_md_redirects_to_agents_md(dir.path()),
            "an @AGENTS.md line a sibling's block merely quotes is documentation, \
             not this project's redirect"
        );
        install_instructions(dir.path()).unwrap();
        assert!(
            read(dir.path(), "CLAUDE.md").contains(START_PREFIX),
            "dual-write must be unchanged when there is no real redirect"
        );
        assert!(read(dir.path(), "AGENTS.md").contains(START_PREFIX));
    }

    #[test]
    fn no_redirect_keeps_dual_write_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "CLAUDE.md",
            "# Project\n\nSee @AGENTS.md for details.\n",
        );
        assert!(!claude_md_redirects_to_agents_md(dir.path()));
        install_instructions(dir.path()).unwrap();
        for name in TARGET_FILES {
            assert!(
                read(dir.path(), name).contains(START_PREFIX),
                "{name} lost its block"
            );
        }
        assert_eq!(instructions_state(dir.path()), InstructionsState::UpToDate);
    }

    #[test]
    fn redirect_install_is_idempotent_and_byte_stable() {
        let dir = tempfile::tempdir().unwrap();
        install_instructions(dir.path()).unwrap();
        let seeded = read(dir.path(), "CLAUDE.md");
        write(
            dir.path(),
            "CLAUDE.md",
            &format!("# Project\n\n@AGENTS.md\n\n{seeded}"),
        );

        assert!(install_instructions(dir.path()).unwrap().changed);
        let (claude_1, agents_1) = (read(dir.path(), "CLAUDE.md"), read(dir.path(), "AGENTS.md"));

        let second = install_instructions(dir.path()).unwrap();
        assert!(
            !second.changed,
            "a converged redirect install must be a no-op"
        );
        assert_eq!(
            read(dir.path(), "CLAUDE.md"),
            claude_1,
            "CLAUDE.md not byte-stable"
        );
        assert_eq!(
            read(dir.path(), "AGENTS.md"),
            agents_1,
            "AGENTS.md not byte-stable"
        );
    }

    /// Removal is deliberately more conservative than injection: injection can
    /// always fall back to an append (which deletes nothing), removal cannot.
    #[test]
    fn removal_refuses_every_unprovable_ownership_case_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");

        // 1. Split brain: two own blocks.
        let two = format!("@AGENTS.md\n\n{}\n{}\n", render_block(), render_block());
        std::fs::write(&path, &two).unwrap();
        assert!(matches!(
            remove_instructions(&path).unwrap(),
            RemoveOutcome::Refused(_)
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            two,
            "split brain was written to"
        );

        // 2. Dangling own open with no close.
        let dangling = format!("@AGENTS.md\n\n{START_PREFIX}:v0:deadbeef -->\norphan body\n");
        std::fs::write(&path, &dangling).unwrap();
        assert!(matches!(
            remove_instructions(&path).unwrap(),
            RemoveOutcome::Refused(_)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), dangling);

        // 3. Own close beyond a real foreign block: deleting to it would swallow
        //    the sibling.
        let beyond = format!(
            "@AGENTS.md\n\n{START_PREFIX}:v0:deadbeef -->\nbody\n{FILIGREE_BLOCK}{END_MARKER}\n"
        );
        std::fs::write(&path, &beyond).unwrap();
        assert!(matches!(
            remove_instructions(&path).unwrap(),
            RemoveOutcome::Refused(_)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), beyond);

        // 4. No own block at all: nothing to do, and nothing written.
        let plain = "@AGENTS.md\n";
        std::fs::write(&path, plain).unwrap();
        assert_eq!(remove_instructions(&path).unwrap(), RemoveOutcome::Nothing);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), plain);
    }

    #[test]
    fn removal_deletes_a_file_that_held_nothing_but_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, format!("{}\n", render_block())).unwrap();
        assert_eq!(remove_instructions(&path).unwrap(), RemoveOutcome::Removed);
        assert!(
            !path.exists(),
            "removing the only content is the symmetric inverse of the create"
        );
    }

    #[test]
    fn unreadable_or_symlinked_claude_md_reads_as_no_redirect() {
        let dir = tempfile::tempdir().unwrap();
        // Absent.
        assert!(!claude_md_redirects_to_agents_md(dir.path()));
        // Non-UTF-8.
        std::fs::write(dir.path().join("CLAUDE.md"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(!claude_md_redirects_to_agents_md(dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_claude_md_reads_as_no_redirect_and_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.md");
        std::fs::write(&real, REDIRECT_CLAUDE_MD).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("CLAUDE.md")).unwrap();

        assert!(
            !claude_md_redirects_to_agents_md(dir.path()),
            "a symlinked CLAUDE.md fails SAFE: no redirect, dual-write unchanged"
        );
        let report = install_instructions(dir.path()).unwrap();
        assert_eq!(report.skipped_symlinks.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            REDIRECT_CLAUDE_MD,
            "the symlink target must not be written through"
        );
        assert!(read(dir.path(), "AGENTS.md").contains(START_PREFIX));
    }

    #[test]
    fn masking_hides_own_and_foreign_blocks_and_an_unclosed_block_masks_to_eof() {
        let own = format!("{START_PREFIX}:v0:deadbeef -->\n@AGENTS.md\n{END_MARKER}\n");
        assert!(
            !super::mask_managed_blocks(&own)
                .lines()
                .any(super::is_agents_md_redirect),
            "an @AGENTS.md line inside our OWN block is not the project's redirect"
        );

        let unclosed = "<!-- filigree:instructions:v3:abc -->\nprose\n@AGENTS.md\n";
        assert!(
            !super::mask_managed_blocks(unclosed)
                .lines()
                .any(super::is_agents_md_redirect),
            "an unclosed block masks to EOF: we cannot prove where it ends"
        );

        let outside = format!("@AGENTS.md\n{FILIGREE_BLOCK}");
        assert!(
            super::mask_managed_blocks(&outside)
                .lines()
                .any(super::is_agents_md_redirect),
            "a redirect line outside every block must still be seen"
        );
    }
}
