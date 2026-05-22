# Clarion v1.0 Storage Operations

Operator-facing reference for the constraints around Clarion's local-state
directory. v1.0 is local-first; the storage subsystem is plain SQLite under
`.clarion/`. The constraints below come straight out of
[ADR-011](../adr/ADR-011-storage-architecture.md) (writer-actor + reader-pool
over SQLite) and the v1.0 tag-cut gap-register entries DOC-11, STO-01,
STO-04, and STO-05.

## 1. Local-first storage layout

Per ADR-011, every Clarion project keeps its state in a `.clarion/`
directory at the project root:

```
.clarion/
├── clarion.db              SQLite database (entities, edges, runs, findings, summary_cache)
├── clarion.db-wal          SQLite WAL companion file
├── clarion.db-shm          SQLite shared-memory file
├── clarion.lock            fs2 advisory lock file (writer claim)
├── instance_id             Stable per-project instance ID
└── ...                     plugin caches, scanner baseline, etc.
```

There is no central server, no shared registry, no networked state.
`clarion install --path` creates `.clarion/` on a new project root;
`clarion analyze` and `clarion serve` both read and (in `analyze`'s case)
write into it.

## 2. NFS is prohibited

**`.clarion/` MUST live on a local filesystem.** Do not place a project root
on NFS, SMB, sshfs, or any other network filesystem and expect `clarion
analyze` or `clarion serve` to behave correctly.

The two specific failure modes:

- **POSIX advisory locks over NFS are unreliable.** The `fs2` exclusive
  lock that protects against concurrent analyzers (§3) is silently
  no-op or partially honoured on most NFS configurations. Two analyzers
  on two clients can both believe they hold the lock.
- **SQLite WAL mode is not safe over network filesystems.** The SQLite
  manual explicitly warns that WAL requires shared-memory primitives the
  kernel does not provide across NFS. A WAL-mode database opened over
  NFS can lose committed writes or corrupt the database file.

If your only available storage is networked, do not use Clarion against
that workspace. Clone the project to local disk first.

## 3. No double-analyze

Only one `clarion analyze` may run against a given project root at a time.
The v1.0 binary enforces this with an exclusive `fs2` advisory lock on
`.clarion/clarion.lock`, acquired at the start of `analyze` and held for the
writer-actor lifetime. A second `clarion analyze` against the same
`.clarion/` fails fast with a clear "another clarion analyze is in progress
against this project" error rather than racing the first analyzer's run
state. (See STO-01 in the v1.0 tag-cut gap register for the originating
finding.)

The lock is process-scoped, not user-scoped: the same user starting two
analyzers in two terminals is the typical mistake the lock prevents.

`clarion serve` is read-only against the database and does not contend with
`analyze` on the writer lock, but it shares the same database file via the
reader pool and will observe whichever state `analyze` last committed.

**Same-process restart caveat (STO-05):** the fs2 lock is released when
the analyzer process exits, including on crash. If the previous analyzer
exited abnormally, the next `clarion analyze` will sweep
`runs.status='running'` rows to `'failed'` (see
`recover_preexisting_running_runs`). There is no PID column or heartbeat
on `runs` at v1.0 — a same-host restart cannot distinguish "previous
analyzer crashed" from "previous analyzer is still alive but unlocked
during a brief teardown window". v1.0 mitigates this with the fs2 lock;
the `runs.owner_pid` + `heartbeat_at` schema additions are a v1.1
follow-up.

## 4. Supported backup procedure

The v1.0 supported backup procedure is a four-step shutdown-and-copy. There
is no live `clarion db backup` subcommand at v1.0 (deferred to v1.1, §6).

1. **Ensure no `clarion analyze` is running** against the project root.
   The fs2 advisory lock on `.clarion/clarion.lock` will be released; a
   subsequent `clarion analyze` from a backup script would otherwise race
   with the backup.

2. **Stop `clarion serve` or wait for it to be idle.** `serve` only reads,
   so a torn copy from a running `serve` is less catastrophic than from
   `analyze`, but a clean shutdown is required for the WAL checkpoint to
   complete.

3. **Force the WAL into the main database file** so a plain file copy
   captures all committed state:

   ```bash
   sqlite3 .clarion/clarion.db "PRAGMA wal_checkpoint(TRUNCATE);"
   ```

   `TRUNCATE` mode is the strongest checkpoint — it flushes the WAL into
   `clarion.db` and resets `clarion.db-wal` to zero length.

4. **Copy `.clarion/` to the backup location** with any standard tool
   (`cp -a`, `rsync -a`, `tar`). All three of `clarion.db`,
   `clarion.db-wal`, and `clarion.db-shm` should be present in the copy;
   after a successful TRUNCATE the WAL is empty but the file should still
   be copied.

## 5. Restore

To restore a backup:

1. Stop any running `clarion analyze` or `clarion serve` against the
   project root.
2. Replace the project's `.clarion/` directory with the backup copy. The
   `instance_id` file inside `.clarion/` is part of the backup; restoring
   it preserves the project's federation identity (`/api/v1/_capabilities`
   `instance_id` stays stable across the restore).
3. Run `clarion analyze` to validate the restored database. A fresh analyze
   re-applies any pending migrations and exercises the integrity-check path
   on the e2e test surface.

## 6. v1.1 follow-up: `clarion db backup` subcommand

A live-safe `clarion db backup <path>` subcommand backed by SQLite's online
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
