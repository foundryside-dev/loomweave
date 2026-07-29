//! Isolated worktree-index store lifecycle: create, validate, and — when
//! validation fails — delete-and-rebuild a linked worktree's isolated store.
//!
//! [`ensure_isolated_store`] is the single entry point every caller that
//! resolves a [`WorktreeContext`] must run before treating
//! `ctx.effective_store` as usable: for
//! [`WorktreeKind::Linked`][loomweave_core::worktree::WorktreeKind::Linked]
//! that directory is not `install`-managed the way the primary checkout's
//! store is, so nothing else creates it. `loomweave analyze` (routed
//! unconditionally through [`WorktreeContext::resolve`] — see `analyze.rs`)
//! and `loomweave worktree analyze` (`crate::worktree::cmd`) both call this
//! before opening the database.
//!
//! Store creation is deliberately unceremonious — `create_dir_all` + a plain
//! `serde_json` `metadata.json` + an eagerly-initialised (schema-applied,
//! empty) `loomweave.db` — because everything here is a rebuildable cache
//! (a ~20-30 minute `analyze` re-run recreates it). The one piece of
//! discipline that *is* load-bearing is deletion: metadata that fails to
//! parse, or whose `source_root` no longer matches the resolved worktree
//! (compare canonicalized), triggers a delete-and-rebuild — and that
//! deletion always routes through [`crate::worktree::confine`]'s confined
//! primitive, never a bare `remove_dir_all`. [`DeleteOutcome::Refused`] and
//! [`DeleteOutcome::UnsupportedPlatform`] are both propagated as errors here,
//! never silently treated as "proceed as if deleted" — a dropped refusal
//! masquerading as success is exactly the failure mode Task 2's review
//! flagged.
//!
//! The on-disk metadata shape mirrors the design doc's `On-disk layout` /
//! `Metadata` sections (`docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md`)
//! exactly: `schema`, `stable_id`, `git_admin_identity`, `source_root`,
//! `created_at`. `git_admin_identity` is informational only — recorded for
//! operator/debugging visibility, never read back into a validity or
//! mismatch decision. The only question this file answers programmatically
//! is "is this store still describing the worktree I think it is?", and
//! that question is answered entirely by `source_root` (compared
//! canonicalized); `stable_id` is derived directly from the `BLAKE3` hash
//! the resolver already computed, not re-derived from the recorded
//! `git_admin_identity` string.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use loomweave_core::worktree::{StorePaths, WorktreeContext};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;
use tracing::{info, warn};

use crate::worktree::confine::{DeleteOutcome, WorktreesRoot};

/// `metadata.json`'s `schema` field — bump on an incompatible on-disk shape
/// change (a mismatch here is treated exactly like an unreadable file:
/// delete and rebuild).
const METADATA_SCHEMA: &str = "loomweave.worktree-index.v1";

const METADATA_FILE_NAME: &str = "metadata.json";
/// `<repository-store>/worktrees/` — shared with `crate::worktree::sweep`
/// (the cleanup sweep enumerates the same directory this module creates
/// stores under) and, cross-crate, with `loomweave-cli`'s bin target
/// (`install.rs`'s `--force` guard and `doctor.rs`'s additive worktree-store
/// report both need the same directory name; worktree-index Task 7), so this
/// is `pub` rather than `pub(crate)`.
pub const WORKTREES_DIR_NAME: &str = "worktrees";

const ISO8601_MILLIS_UTC: &[time::format_description::FormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

fn iso8601_now() -> String {
    OffsetDateTime::now_utc()
        .format(ISO8601_MILLIS_UTC)
        .expect("fixed ISO-8601 format description should format")
}

/// On-disk shape of `<worktree-store>/metadata.json`.
///
/// Plain `serde_json`, no checksum, no journal — see the module docs. Any
/// field this fails to deserialize is treated identically to a completely
/// unreadable file. `git_admin_identity` is informational only: read back
/// into this struct like every other field (an unreadable file is still
/// unreadable), but never compared against anything — see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Metadata {
    schema: String,
    stable_id: String,
    git_admin_identity: String,
    source_root: String,
    created_at: String,
}

/// What [`ensure_isolated_store`] did to reach a valid store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    /// `ctx.kind` was not [`WorktreeKind::Linked`][kind] — every other kind
    /// uses its own, already-`install`ed store; nothing here applies.
    ///
    /// [kind]: loomweave_core::worktree::WorktreeKind::Linked
    NotIsolated,
    /// No store directory existed yet at `ctx.effective_store`; created
    /// fresh.
    Created,
    /// Valid metadata already described this exact worktree; nothing was
    /// touched.
    Reused,
    /// Metadata was unreadable, or described a different worktree; the
    /// directory was deleted (via the confined primitive) and rebuilt.
    /// `reason` is the human-readable cause, also present in the log line
    /// this emits.
    Rebuilt {
        /// Why the prior store was rejected.
        reason: String,
    },
}

/// Everything that can go wrong ensuring a worktree's isolated store exists.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// `<repository-store>/worktrees/` could not be created.
    #[error("create worktrees directory {path}: {source}")]
    CreateWorktreesDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// [`WorktreesRoot::open`] failed to pin a handle to `worktrees/`.
    #[error("open worktrees directory {path}: {source}")]
    OpenWorktreesRoot {
        /// The directory that could not be opened.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// The store's own directory (`worktrees/<stable-id>/`) could not be
    /// created.
    #[error("create store directory {path}: {source}")]
    CreateStoreDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// `metadata.json` could not be serialized. Should not happen in
    /// practice — the internal `Metadata` type has no field that can fail to
    /// serialize — but `serde_json::to_vec_pretty` is fallible, so this is
    /// modeled honestly rather than `.expect()`-ed away.
    #[error("serialize store metadata: {0}")]
    SerializeMetadata(#[from] serde_json::Error),
    /// `metadata.json` could not be written.
    #[error("write metadata {path}: {source}")]
    WriteMetadata {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// The fresh `loomweave.db` could not be opened, or its write pragmas /
    /// schema migrations could not be applied.
    #[error("initialise store database {path}: {source}")]
    InitDb {
        /// The database file being initialised.
        path: PathBuf,
        /// The underlying storage failure.
        source: loomweave_storage::StorageError,
    },
    /// The confined-deletion primitive refused to delete the stale store.
    /// Nothing was deleted; the isolated store is left exactly as it was —
    /// this is propagated as an error, never treated as "proceed as if
    /// deleted" (see the module docs).
    #[error("refused to delete stale worktree store {stable_id}: {reason}")]
    DeleteRefused {
        /// The stable ID (directory name) that was refused.
        stable_id: String,
        /// The confined primitive's refusal reason.
        reason: String,
    },
    /// This platform (or kernel) has no confined-deletion mechanism, so the
    /// stale store could not be safely rebuilt.
    #[error(
        "cannot rebuild worktree store {stable_id}: no confined-deletion \
         mechanism is available on this platform, so the stale store was \
         left untouched"
    )]
    DeleteUnsupportedPlatform {
        /// The stable ID (directory name) that could not be rebuilt.
        stable_id: String,
    },
}

/// Ensure `ctx`'s isolated worktree store exists on disk and describes the
/// worktree `ctx` was resolved from.
///
/// A no-op ([`StoreOutcome::NotIsolated`]) unless `ctx.kind` is
/// [`WorktreeKind::Linked`][kind] — callers should still call this
/// unconditionally rather than branch on `ctx.kind` themselves, so the
/// no-isolation-needed case stays exactly as cheap as every other outcome to
/// reason about at the call site.
///
/// Callers must ensure `ctx.repository_store` (the primary checkout's own
/// store) already exists — an un-`install`ed primary is a distinct,
/// caller-facing error this function does not itself diagnose.
///
/// # Errors
///
/// Returns [`StoreError`] if any filesystem or database operation fails, or
/// if a stale store needed to be deleted and the confined primitive refused
/// or reported no platform support — see [`StoreError::DeleteRefused`] and
/// [`StoreError::DeleteUnsupportedPlatform`].
///
/// # Panics
///
/// Never in practice: `ctx.source_root` is only non-UTF-8-checked at
/// construction, and [`WorktreeContext::resolve`] already rejects a
/// non-UTF-8 `source_root` before a `WorktreeContext` can exist, so the
/// internal `.expect()` on `source_root.to_str()` cannot fail for any `ctx`
/// obtained the normal way. Likewise, `WorktreeContext::resolve` only ever
/// sets `stable_id` and `git_admin_identity` together (both `Some` for
/// [`WorktreeKind::Linked`][kind], both `None` otherwise), so the internal
/// `.expect()` on `git_admin_identity` after already matching `stable_id` as
/// `Some` cannot fail for any `ctx` obtained the normal way.
///
/// [kind]: loomweave_core::worktree::WorktreeKind::Linked
pub fn ensure_isolated_store(ctx: &WorktreeContext) -> Result<StoreOutcome, StoreError> {
    let Some(stable_id) = ctx.stable_id.as_deref() else {
        return Ok(StoreOutcome::NotIsolated);
    };
    let admin_identity = ctx
        .git_admin_identity
        .as_deref()
        .expect("WorktreeContext sets git_admin_identity whenever stable_id is set");

    let worktrees_dir = ctx.repository_store.join(WORKTREES_DIR_NAME);
    fs::create_dir_all(&worktrees_dir).map_err(|source| StoreError::CreateWorktreesDir {
        path: worktrees_dir.clone(),
        source,
    })?;
    let root =
        WorktreesRoot::open(&worktrees_dir).map_err(|source| StoreError::OpenWorktreesRoot {
            path: worktrees_dir.clone(),
            source,
        })?;

    let candidate = worktrees_dir.join(stable_id);
    // `WorktreeContext::resolve` already rejects a non-UTF-8 `source_root`
    // before this context could exist.
    let source_root = ctx
        .source_root
        .to_str()
        .expect("WorktreeContext guarantees a UTF-8 source_root")
        .to_owned();

    let reason = match read_metadata(&candidate) {
        MetadataState::Absent => {
            create_fresh(&candidate, stable_id, admin_identity, &source_root)?;
            return Ok(StoreOutcome::Created);
        }
        MetadataState::Valid(meta)
            if meta.schema == METADATA_SCHEMA
                && meta.stable_id == stable_id
                && meta.source_root == source_root =>
        {
            return Ok(StoreOutcome::Reused);
        }
        // Name exactly which of the three validity checks failed
        // (clarion-73874f5939): this reason is the delete-and-rebuild's only
        // audit trail, and a blanket "source_root mismatch" on a schema bump
        // reads as self-contradictory when both paths are identical.
        MetadataState::Valid(meta) => {
            let mut mismatches = Vec::new();
            if meta.schema != METADATA_SCHEMA {
                mismatches.push(format!(
                    "schema {:?} != expected {METADATA_SCHEMA:?}",
                    meta.schema
                ));
            }
            if meta.stable_id != stable_id {
                mismatches.push(format!(
                    "stable_id {:?} != resolved {stable_id:?}",
                    meta.stable_id
                ));
            }
            if meta.source_root != source_root {
                mismatches.push(format!(
                    "source_root {:?} != resolved worktree {source_root:?}",
                    meta.source_root
                ));
            }
            format!("metadata.json mismatch: {}", mismatches.join("; "))
        }
        MetadataState::Unreadable(detail) => format!("unreadable metadata.json: {detail}"),
    };

    warn!(
        stable_id,
        reason = %reason,
        "worktree-isolated store failed validation; deleting and rebuilding"
    );
    match root.delete_worktree_store(stable_id, &reason) {
        DeleteOutcome::Deleted => {}
        DeleteOutcome::Refused(refusal) => {
            return Err(StoreError::DeleteRefused {
                stable_id: stable_id.to_owned(),
                reason: refusal.to_string(),
            });
        }
        DeleteOutcome::UnsupportedPlatform => {
            return Err(StoreError::DeleteUnsupportedPlatform {
                stable_id: stable_id.to_owned(),
            });
        }
    }

    create_fresh(&candidate, stable_id, admin_identity, &source_root)?;
    Ok(StoreOutcome::Rebuilt { reason })
}

/// The result of reading `<candidate>/metadata.json`.
enum MetadataState {
    /// The store directory does not exist yet — first-time creation, no
    /// delete-and-rebuild needed. (A directory that exists but has no
    /// `metadata.json` reads as [`Self::Unreadable`] and is rebuilt.)
    Absent,
    /// The directory exists, but `metadata.json` is missing, unreadable, or
    /// fails to parse. `String` is a human-readable detail for the rebuild
    /// reason.
    Unreadable(String),
    /// `metadata.json` parsed successfully.
    Valid(Metadata),
}

fn read_metadata(candidate: &Path) -> MetadataState {
    if !candidate.exists() {
        return MetadataState::Absent;
    }
    let path = candidate.join(METADATA_FILE_NAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => return MetadataState::Unreadable(err.to_string()),
    };
    match serde_json::from_slice::<Metadata>(&bytes) {
        Ok(meta) => MetadataState::Valid(meta),
        Err(err) => MetadataState::Unreadable(err.to_string()),
    }
}

/// Create `candidate` fresh: the directory, `metadata.json`, and an
/// empty-initialized `loomweave.db` (schema applied, no data) — see the
/// module docs for why the database is touched eagerly rather than left for
/// `analyze`'s own migration step to create.
fn create_fresh(
    candidate: &Path,
    stable_id: &str,
    admin_identity: &str,
    source_root: &str,
) -> Result<(), StoreError> {
    fs::create_dir_all(candidate).map_err(|source| StoreError::CreateStoreDir {
        path: candidate.to_path_buf(),
        source,
    })?;

    let metadata = Metadata {
        schema: METADATA_SCHEMA.to_owned(),
        stable_id: stable_id.to_owned(),
        git_admin_identity: admin_identity.to_owned(),
        source_root: source_root.to_owned(),
        created_at: iso8601_now(),
    };
    let metadata_path = candidate.join(METADATA_FILE_NAME);
    let json = serde_json::to_vec_pretty(&metadata)?;
    fs::write(&metadata_path, json).map_err(|source| StoreError::WriteMetadata {
        path: metadata_path.clone(),
        source,
    })?;

    let store_paths = StorePaths::under(candidate);
    initialise_db(&store_paths.db)?;

    info!(
        stable_id,
        source_root,
        path = %candidate.display(),
        "worktree-isolated store ready"
    );
    Ok(())
}

/// Open (creating) `db_path` and apply write pragmas + schema migrations —
/// the same sequence `loomweave install` uses for the primary store's
/// `loomweave.db`, so a worktree store's database is never a bare, unmigrated
/// `SQLite` file. Empty (no rows) until the next `analyze` populates it.
fn initialise_db(db_path: &Path) -> Result<(), StoreError> {
    let to_init_err = |source: loomweave_storage::StorageError| StoreError::InitDb {
        path: db_path.to_path_buf(),
        source,
    };
    let mut conn = Connection::open(db_path)
        .map_err(loomweave_storage::StorageError::from)
        .map_err(to_init_err)?;
    loomweave_storage::pragma::apply_write_pragmas(&conn).map_err(to_init_err)?;
    loomweave_storage::schema::apply_migrations(&mut conn).map_err(to_init_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{METADATA_FILE_NAME, StoreOutcome, ensure_isolated_store};
    use loomweave_core::worktree::WorktreeContext;
    use std::path::Path;
    use std::process::Command;

    /// Run `git -C <dir> <args>`, panicking on failure — test setup only.
    /// Mirrors `loomweave-core`'s own `worktree_context.rs` fixture helper.
    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?} in {}: {e}", dir.display()));
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", branch]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "hi\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "init"]);
    }

    /// Build a real primary repo with one linked worktree, and resolve the
    /// linked worktree's [`WorktreeContext`].
    fn linked_context(root: &Path) -> WorktreeContext {
        let repo = root.join("repo");
        init_repo(&repo, "main");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", "../linked"],
        );
        let linked = root.join("linked");
        WorktreeContext::resolve(&linked).expect("resolves as Linked")
    }

    #[test]
    fn creating_a_store_writes_plain_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());

        let outcome = ensure_isolated_store(&ctx).expect("ensure store");
        assert_eq!(outcome, StoreOutcome::Created);

        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        let raw = std::fs::read_to_string(&metadata_path).expect("read metadata.json");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(value["schema"], "loomweave.worktree-index.v1");
        assert_eq!(value["stable_id"], ctx.stable_id.clone().unwrap());
        assert_eq!(
            value["git_admin_identity"],
            ctx.git_admin_identity.clone().unwrap(),
            "metadata.json must record the Git administrative identity per the design's v1 schema"
        );
        assert_eq!(
            value["source_root"],
            ctx.source_root.to_str().unwrap().to_owned()
        );
        assert!(value["created_at"].is_string());

        assert!(
            ctx.store_paths.db.is_file(),
            "loomweave.db must be eagerly created: {}",
            ctx.store_paths.db.display()
        );
        let conn = rusqlite::Connection::open(&ctx.store_paths.db).unwrap();
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert!(
            user_version > 0,
            "eagerly-created loomweave.db must have schema applied (user_version > 0)"
        );
    }

    #[test]
    fn unreadable_metadata_deletes_and_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        ensure_isolated_store(&ctx).expect("first ensure");

        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        std::fs::write(&metadata_path, b"not json at all {{{").unwrap();

        let outcome = ensure_isolated_store(&ctx).expect("second ensure");
        assert!(
            matches!(outcome, StoreOutcome::Rebuilt { .. }),
            "expected Rebuilt, got {outcome:?}"
        );

        let raw = std::fs::read_to_string(&metadata_path).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("rebuilt metadata is valid JSON");
        assert_eq!(value["schema"], "loomweave.worktree-index.v1");
    }

    #[test]
    fn source_root_mismatch_deletes_and_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        ensure_isolated_store(&ctx).expect("first ensure");

        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        let raw = std::fs::read_to_string(&metadata_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["source_root"] = serde_json::Value::String("/nowhere/stale-path".to_owned());
        std::fs::write(&metadata_path, serde_json::to_vec(&value).unwrap()).unwrap();

        let outcome = ensure_isolated_store(&ctx).expect("second ensure");
        assert!(
            matches!(outcome, StoreOutcome::Rebuilt { .. }),
            "expected Rebuilt, got {outcome:?}"
        );

        let raw = std::fs::read_to_string(&metadata_path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["source_root"],
            ctx.source_root.to_str().unwrap().to_owned(),
            "rebuilt metadata must describe the ACTUAL resolved worktree, not the stale one"
        );
    }

    #[test]
    fn shared_override_does_not_delete_another_repositorys_live_store() {
        let tmp = tempfile::tempdir().unwrap();
        let shared_store = tmp.path().join("shared-store");
        let mut contexts = Vec::new();

        for parent in ["a", "b"] {
            let repo = tmp.path().join(parent).join("repo");
            init_repo(&repo, "main");
            std::fs::write(
                repo.join("weft.toml"),
                format!(
                    "[loomweave]\nstore_dir = {:?}\n",
                    shared_store.to_str().unwrap()
                ),
            )
            .unwrap();
            git(&repo, &["add", "weft.toml"]);
            git(&repo, &["commit", "-qm", "configure shared store"]);
            git(
                &repo,
                &["worktree", "add", "-q", "-b", "feature", "../linked"],
            );
            contexts.push(
                WorktreeContext::resolve(&tmp.path().join(parent).join("linked"))
                    .expect("resolve linked worktree"),
            );
        }
        std::fs::create_dir_all(&shared_store).unwrap();

        ensure_isolated_store(&contexts[0]).expect("create first repository store");
        let marker = contexts[0].effective_store.join("first-repository.marker");
        std::fs::write(&marker, "live\n").unwrap();
        ensure_isolated_store(&contexts[1]).expect("create second repository store");

        assert!(
            marker.is_file(),
            "initializing the second repository must not rebuild-delete the first repository's live index"
        );
        assert_ne!(contexts[0].effective_store, contexts[1].effective_store);
    }

    #[test]
    fn removed_and_readded_worktree_name_rebuilds_not_reuses() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");

        std::fs::create_dir_all(tmp.path().join("outer")).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "first",
                "../outer/reused-name",
            ],
        );
        let first_linked = tmp.path().join("outer").join("reused-name");
        let first_ctx = WorktreeContext::resolve(&first_linked).expect("resolves as Linked");
        let outcome = ensure_isolated_store(&first_ctx).expect("ensure first");
        assert_eq!(outcome, StoreOutcome::Created);
        let first_stable_id = first_ctx.stable_id.clone().unwrap();

        git(&repo, &["worktree", "remove", "-f", "../outer/reused-name"]);

        std::fs::create_dir_all(tmp.path().join("inner")).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "second",
                "../inner/reused-name",
            ],
        );
        let second_linked = tmp.path().join("inner").join("reused-name");
        let second_ctx = WorktreeContext::resolve(&second_linked).expect("resolves as Linked");

        // Same admin name ("reused-name") ⇒ same stable_id (derived only
        // from the admin-identity path fragment), but a DIFFERENT
        // source_root — this is the exact scenario `source_root_mismatch`
        // guards, reached here through real `git worktree remove`/`add`
        // rather than a hand-forged metadata.json.
        assert_eq!(
            first_stable_id,
            second_ctx.stable_id.clone().unwrap(),
            "sanity: reusing the admin name must reuse the stable_id"
        );
        assert_ne!(first_ctx.source_root, second_ctx.source_root);

        let outcome = ensure_isolated_store(&second_ctx).expect("ensure second");
        assert!(
            matches!(outcome, StoreOutcome::Rebuilt { .. }),
            "a re-added worktree under the same admin name must rebuild, not reuse: {outcome:?}"
        );

        let metadata_path = second_ctx.effective_store.join(METADATA_FILE_NAME);
        let raw = std::fs::read_to_string(&metadata_path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["source_root"],
            second_ctx.source_root.to_str().unwrap().to_owned()
        );
    }

    #[test]
    fn delete_and_rebuild_logs_the_reason() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);

        impl Write for SharedBuf {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("lock log buffer")
                    .extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for SharedBuf {
            type Writer = SharedBuf;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        ensure_isolated_store(&ctx).expect("first ensure");
        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        std::fs::write(&metadata_path, b"garbage").unwrap();

        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .finish();

        let outcome = tracing::subscriber::with_default(subscriber, || {
            ensure_isolated_store(&ctx).expect("rebuild")
        });

        let StoreOutcome::Rebuilt { reason } = outcome else {
            panic!("expected Rebuilt, got {outcome:?}");
        };
        let log = String::from_utf8(buf.0.lock().expect("lock log buffer").clone())
            .expect("log is UTF-8");
        assert!(
            log.contains(ctx.stable_id.as_deref().unwrap()),
            "log line must include the stable ID: {log}"
        );
        assert!(
            log.contains(&reason),
            "log line must include the rebuild reason {reason:?}: {log}"
        );
    }

    /// clarion-73874f5939: a schema-only mismatch must be reported as a
    /// schema mismatch — the fall-through arm used to label every parsed
    /// mismatch `"source_root mismatch"`, producing a self-contradictory
    /// diagnostic (`describes "/same/path", resolved worktree is
    /// "/same/path"`) on the first `METADATA_SCHEMA` bump.
    #[test]
    fn schema_mismatch_reason_names_the_failing_check() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        assert_eq!(
            ensure_isolated_store(&ctx).expect("create"),
            StoreOutcome::Created
        );

        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        let raw = std::fs::read_to_string(&metadata_path).expect("read metadata");
        let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse metadata");
        value["schema"] = serde_json::Value::String("loomweave.worktree-index.v0".to_owned());
        std::fs::write(
            &metadata_path,
            serde_json::to_string(&value).expect("serialize"),
        )
        .expect("write stale-schema metadata");

        let outcome = ensure_isolated_store(&ctx).expect("rebuild");
        let StoreOutcome::Rebuilt { reason } = outcome else {
            panic!("expected Rebuilt, got {outcome:?}");
        };
        assert!(
            reason.contains("schema"),
            "the reason must name the failing check: {reason}"
        );
        assert!(
            !reason.contains("source_root"),
            "a schema-only mismatch must not claim a source_root mismatch: {reason}"
        );
    }

    #[test]
    fn same_identity_same_root_reuses_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        let first = ensure_isolated_store(&ctx).expect("first ensure");
        assert_eq!(first, StoreOutcome::Created);

        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        let created_at_first = {
            let raw = std::fs::read_to_string(&metadata_path).unwrap();
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            value["created_at"].as_str().unwrap().to_owned()
        };

        // A marker only present if the store survives untouched.
        let marker = ctx.effective_store.join("untouched.marker");
        std::fs::write(&marker, b"still here").unwrap();

        let second = ensure_isolated_store(&ctx).expect("second ensure");
        assert_eq!(second, StoreOutcome::Reused);

        assert!(
            marker.is_file(),
            "reusing an already-valid store must not delete-and-rebuild it"
        );
        let created_at_second = {
            let raw = std::fs::read_to_string(&metadata_path).unwrap();
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            value["created_at"].as_str().unwrap().to_owned()
        };
        assert_eq!(
            created_at_first, created_at_second,
            "metadata.json must not be rewritten on a plain reuse"
        );
    }

    #[test]
    fn main_store_path_is_untouched_by_worktree_creation() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());

        // Pre-`install` the primary's own store — mirrors what `loomweave
        // install` actually leaves behind (a schema-applied, empty
        // loomweave.db) — so the assertions below prove creating the
        // WORKTREE store leaves that content untouched, rather than merely
        // observing it doesn't exist yet.
        std::fs::create_dir_all(&ctx.repository_store).unwrap();
        let primary_db = ctx.repository_store.join("loomweave.db");
        super::initialise_db(&primary_db).expect("initialise primary db");
        let primary_db_before = std::fs::read(&primary_db).unwrap();
        let primary_entries_before: std::collections::BTreeSet<String> =
            std::fs::read_dir(&ctx.repository_store)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();

        ensure_isolated_store(&ctx).expect("ensure worktree store");

        assert!(
            !ctx.repository_store.join(METADATA_FILE_NAME).is_file(),
            "the primary checkout's own store directory has no metadata.json of its own"
        );
        assert!(
            ctx.effective_store
                .starts_with(ctx.repository_store.join("worktrees")),
            "the isolated store must live strictly under repository_store/worktrees/"
        );
        assert_eq!(
            std::fs::read(&primary_db).unwrap(),
            primary_db_before,
            "creating a worktree store must not modify the primary's own loomweave.db bytes"
        );
        let primary_entries_after: std::collections::BTreeSet<String> =
            std::fs::read_dir(&ctx.repository_store)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
        let mut expected_after = primary_entries_before;
        expected_after.insert("worktrees".to_owned());
        assert_eq!(
            primary_entries_after, expected_after,
            "the only new entry directly under repository_store must be worktrees/"
        );
    }

    #[test]
    fn non_linked_context_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = WorktreeContext::resolve(tmp.path()).expect("resolves as Standalone");
        let outcome = ensure_isolated_store(&ctx).expect("ensure");
        assert_eq!(outcome, StoreOutcome::NotIsolated);
        assert!(!ctx.effective_store.exists());
    }

    /// Task 2's review named this exact failure mode: a `DeleteOutcome`
    /// that is `Refused` (or `UnsupportedPlatform`) must never be silently
    /// treated as "proceed as if deleted". Plant a symlink inside the store
    /// so the confined primitive's validation pass refuses the whole
    /// deletion, force a rebuild trigger (corrupt metadata.json), and assert
    /// the refusal comes back as a propagated [`StoreError`] with the store
    /// left completely untouched — not silently rebuilt over the refusal.
    #[test]
    #[cfg(target_os = "linux")]
    fn delete_refusal_is_propagated_never_treated_as_success() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        ensure_isolated_store(&ctx).expect("first ensure");

        let metadata_path = ctx.effective_store.join(METADATA_FILE_NAME);
        std::fs::write(&metadata_path, b"garbage-forces-rebuild").unwrap();

        let outside_target = tmp.path().join("outside-target.txt");
        std::fs::write(&outside_target, b"not part of the store").unwrap();
        let evil_link = ctx.effective_store.join("evil-link");
        std::os::unix::fs::symlink(&outside_target, &evil_link).unwrap();

        let err = ensure_isolated_store(&ctx)
            .expect_err("a refused deletion must be a propagated error, never Ok");
        assert!(
            matches!(err, super::StoreError::DeleteRefused { .. }),
            "expected DeleteRefused, got {err:?}"
        );

        // Nothing was deleted: the corrupted metadata and the symlink are
        // both still exactly as they were.
        assert_eq!(
            std::fs::read(&metadata_path).unwrap(),
            b"garbage-forces-rebuild",
            "a refused delete-and-rebuild must leave the stale metadata untouched"
        );
        assert!(
            evil_link.is_symlink(),
            "a refused delete-and-rebuild must leave the store's contents untouched"
        );
    }
}
