# Runtime Topology

Loomweave stores project state in `.loomweave/loomweave.db`. The current v0.1 CLI
uses SQLite WAL mode with a 5 second `busy_timeout` on writer and reader
connections. `loomweave analyze` opens one writer actor for ingest. `loomweave
serve` always opens a reader pool, and opens its own writer actor only when LLM
summary or inferred-edge writes are enabled by `loomweave.yaml`.

These storage settings are implementation constants today, not configurable
`loomweave.yaml` keys:

- write connections set `journal_mode=WAL`, `synchronous=NORMAL`,
  `busy_timeout=5000`, `wal_autocheckpoint=1000`, and `foreign_keys=ON`
- read connections set `busy_timeout=5000` and `foreign_keys=ON`
- analyze and serve writer actors each use a bounded command queue of 256
  operations

## Supported

One `loomweave analyze` process and one `loomweave serve` process may run against
the same `.loomweave/loomweave.db`. `serve` reads use committed SQLite snapshots:
in-flight analyze writes are invisible until their transaction commits and a
later read checks out a connection. If LLM-backed `serve` writes race with
analyze ingest, SQLite serialises the writers and waits up to 5 seconds before
returning a lock error.

This topology is the default local workflow:

```sh
loomweave analyze .
loomweave serve --path .
```

Long analyze runs can make `serve` responses stale relative to the source tree
until the relevant analyze batches commit. For the least surprising results,
start `serve` after a completed analyze run when operators need a stable
snapshot for a review session.

## Unsupported

Do not run multiple `loomweave analyze` processes against the same
`.loomweave/loomweave.db`. Loomweave has one writer actor per process, not one global
writer across processes, so two analyze runs can contend at SQLite's single
writer boundary and produce interleaved run state.

Do not run `loomweave install --force` while either `loomweave analyze` or
`loomweave serve` is using the same project. `--force` replaces `.loomweave/`, so it
is an offline maintenance operation.

Do not delete SQLite sidecar files, copy `.loomweave/loomweave.db` without its WAL
sidecars, or edit `.loomweave/` files while Loomweave is running. Stop the processes
first, then copy or repair the store.

## Not Yet Shipped

ADR-011 describes a future shadow database mode for zero-stale reads during
long analysis runs. The current CLI does not expose a `--shadow-db` flag, so
operators should treat in-place analyze plus WAL as the only shipped v0.1
runtime topology.
