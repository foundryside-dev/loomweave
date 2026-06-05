# Loomweave v1.0 Storage Operations

Operator-facing reference for the constraints around Loomweave's local-state
directory. v1.0 is local-first; the storage subsystem is plain SQLite under
`.loomweave/`. The constraints below come straight out of
[ADR-011](../adr/ADR-011-storage-architecture.md) (writer-actor + reader-pool
over SQLite) and the v1.0 tag-cut gap-register entries DOC-11, STO-01,
STO-04, and STO-05.

## 1. Local-first storage layout

Per ADR-011, every Loomweave project keeps its state in a `.loomweave/`
directory at the project root:

```
.loomweave/
├── loomweave.db              SQLite database (entities, edges, runs, findings, summary_cache)
├── loomweave.db-wal          SQLite WAL companion file
├── loomweave.db-shm          SQLite shared-memory file
├── loomweave.lock            fs2 advisory lock file (writer claim)
├── instance_id             Stable per-project instance ID
└── ...                     plugin caches, scanner baseline, etc.
```

There is no central server, no shared registry, no networked state.
`loomweave install --path` creates `.loomweave/` on a new project root;
`loomweave analyze` and `loomweave serve` both read and (in `analyze`'s case)
write into it.

## 2. NFS is prohibited

**`.loomweave/` MUST live on a local filesystem.** Do not place a project root
on NFS, SMB, sshfs, or any other network filesystem and expect `loomweave
analyze` or `loomweave serve` to behave correctly.

The two specific failure modes:

- **POSIX advisory locks over NFS are unreliable.** The `fs2` exclusive
  lock that protects against concurrent analyzers (§3) is silently
  no-op or partially honoured on most NFS configurations. Two analyzers
  on two clients can both believe they hold the lock.
- **SQLite WAL mode is not safe over network filesystems.** The SQLite
  manual explicitly warns that WAL requires shared-memory primitives the
  kernel does not provide across NFS. A WAL-mode database opened over
  NFS can lose committed writes or corrupt the database file.

If your only available storage is networked, do not use Loomweave against
that workspace. Clone the project to local disk first.

## 3. No double-analyze

Only one `loomweave analyze` may run against a given project root at a time.
The v1.0 binary enforces this with an exclusive `fs2` advisory lock on
`.loomweave/loomweave.lock`, acquired at the start of `analyze` and held for the
writer-actor lifetime. A second `loomweave analyze` against the same
`.loomweave/` fails fast with a clear "another loomweave analyze is in progress
against this project" error rather than racing the first analyzer's run
state. (See STO-01 in the v1.0 tag-cut gap register for the originating
finding.)

The lock is process-scoped, not user-scoped: the same user starting two
analyzers in two terminals is the typical mistake the lock prevents.

`loomweave serve` is read-only against the database and does not contend with
`analyze` on the writer lock, but it shares the same database file via the
reader pool and will observe whichever state `analyze` last committed.

**Same-process restart caveat (STO-05):** the fs2 lock is released when
the analyzer process exits, including on crash. If the previous analyzer
exited abnormally, the next `loomweave analyze` will sweep
`runs.status='running'` rows to `'failed'` (see
`recover_preexisting_running_runs`). There is no PID column or heartbeat
on `runs` at v1.0 — a same-host restart cannot distinguish "previous
analyzer crashed" from "previous analyzer is still alive but unlocked
during a brief teardown window". v1.0 mitigates this with the fs2 lock;
the `runs.owner_pid` + `heartbeat_at` schema additions are a v1.1
follow-up.

## 4. Supported backup procedure

The v1.0 supported backup procedure is a four-step shutdown-and-copy. There
is no live `loomweave db backup` subcommand at v1.0 (deferred to v1.1, §6).

1. **Ensure no `loomweave analyze` is running** against the project root.
   The fs2 advisory lock on `.loomweave/loomweave.lock` will be released; a
   subsequent `loomweave analyze` from a backup script would otherwise race
   with the backup.

2. **Stop `loomweave serve` or wait for it to be idle.** `serve` only reads,
   so a torn copy from a running `serve` is less catastrophic than from
   `analyze`, but a clean shutdown is required for the WAL checkpoint to
   complete.

3. **Force the WAL into the main database file** so a plain file copy
   captures all committed state:

   ```bash
   sqlite3 .loomweave/loomweave.db "PRAGMA wal_checkpoint(TRUNCATE);"
   ```

   `TRUNCATE` mode is the strongest checkpoint — it flushes the WAL into
   `loomweave.db` and resets `loomweave.db-wal` to zero length.

   **Why this step matters:** in WAL mode, committed pages live in
   `loomweave.db-wal` until a checkpoint folds them back into `loomweave.db`. A
   naive `cp .loomweave/loomweave.db backup.db` during (or shortly after) a live
   `analyze` therefore captures a *torn* copy — the main database file is
   missing the most recent committed transactions, which are still sitting in
   the separate `-wal` file. Forcing a `TRUNCATE` checkpoint first guarantees
   `loomweave.db` is self-contained before the copy.

4. **Copy `.loomweave/` to the backup location** with any standard tool
   (`cp -a`, `rsync -a`, `tar`). All three of `loomweave.db`,
   `loomweave.db-wal`, and `loomweave.db-shm` should be present in the copy;
   after a successful TRUNCATE the WAL is empty but the file should still
   be copied.

## 5. Restore

To restore a backup:

1. Stop any running `loomweave analyze` or `loomweave serve` against the
   project root.
2. Replace the project's `.loomweave/` directory with the backup copy. The
   `instance_id` file inside `.loomweave/` is part of the backup; restoring
   it preserves the project's federation identity (`/api/v1/_capabilities`
   `instance_id` stays stable across the restore).
3. Run `loomweave analyze` to validate the restored database. A fresh analyze
   re-applies any pending migrations and exercises the integrity-check path
   on the e2e test surface.

## 6. v1.1 follow-up: `loomweave db backup` subcommand

A live-safe `loomweave db backup <path>` subcommand backed by SQLite's online
backup API is tracked for v1.1. It will replace steps 1–4 of §4 with a
single command that takes a snapshot without requiring `analyze` and
`serve` to be quiesced. Until then, follow §4.

## References

- [ADR-011 — Storage Architecture](../adr/ADR-011-storage-architecture.md)
  — writer-actor and reader-pool design, normative source for §1 and §3.
- [`docs/implementation/v1.0-tag-cut/gap-register.md`](../../implementation/v1.0-tag-cut/gap-register.md)
  — DOC-11, STO-01, STO-04, STO-05 gap entries that drove this document.
- [SQLite WAL mode caveats](https://www.sqlite.org/wal.html#noshm) — the
  upstream warning that underlies the NFS prohibition in §2.
