# CLI reference

The `loomweave` binary has a small, focused command set. Run `loomweave <command>
--help` for the authoritative, version-matched flags.

```text
loomweave <command> [options]
```

## `loomweave install`

Initialise `.loomweave/` and install agent-orientation assets.

A bare `loomweave install` does everything: `.loomweave/` init plus the skill pack
and the `SessionStart` hook. If `.loomweave/` already exists, init is skipped and
skills/hooks are applied idempotently.

| Flag | Effect |
| --- | --- |
| `--path <DIR>` | Directory to install into (default: current directory) |
| `--force` | Overwrite an existing `.loomweave/` directory |
| `--skills` | Install only the bundled `loomweave-workflow` skill pack |
| `--hooks` | Merge only a `SessionStart` hook into `.claude/settings.json` |
| `--all` | Do everything (equivalent to a bare install) |

## `loomweave analyze`

Walk the source tree, dispatch discovered language plugins to extract
entities/edges, and persist results to `.loomweave/loomweave.db`. Re-runs are
idempotent (UPSERT on the entity id) and incremental by default. If no plugins
are on `$PATH`, exits `0` with a warning and status `skipped_no_plugins`.

| Flag | Effect |
| --- | --- |
| `[PATH]` | Path to analyse (default: current directory) |
| `--config <FILE>` | Path to `loomweave.yaml` (default: project-root if present) |
| `--no-incremental` | Force a full re-index, disabling the unchanged-file skip |
| `--resume <RUN_ID>` | Reuse a prior run id; re-emit findings without flipping the peer's prior findings to `unseen_in_latest` |
| `--prune-unseen` | Ask Filigree to soft-archive stale Loomweave findings (enrich-only) |
| `--no-sei` | Skip the stable-entity-identity mint pass (diagnostic escape hatch) |
| `--allow-unredacted-secrets` | Allow analysis of files with unredacted secrets (requires confirmation) |

!!! warning "Analyze writes to the project root"
    `analyze` always persists to the project root's `.loomweave/`, regardless of
    where `--path` pointed during `install`. Wipe a stale `.loomweave/` before
    re-analysing the same corpus from scratch, or use `--no-incremental`.

## `loomweave serve`

Run the MCP stdio server, exposing the consult tools over MCP.

| Flag | Effect |
| --- | --- |
| `--path <DIR>` | Project directory containing `.loomweave/loomweave.db` (default: current directory) |
| `--config <FILE>` | Path to `loomweave.yaml` |

## `loomweave doctor`

Verify (and optionally repair) the installed agent-orientation surfaces: the
`loomweave-workflow` skill pack, the `SessionStart` hook, and the `.mcp.json` MCP
registration. Prints a per-surface report; exits non-zero if any problem
remains, so it works as a CI or pre-commit gate.

| Flag | Effect |
| --- | --- |
| `--path <DIR>` | Project directory to check (default: current directory) |
| `--fix` | Repair problems in place (idempotent). Without it, doctor only reports |

## `loomweave db backup`

Take a consistent, WAL-safe online backup of `.loomweave/loomweave.db`. Unlike `cp`,
this captures outstanding WAL frames into a standalone single-file copy, so it is
safe to run during a live `loomweave analyze`.

| Flag | Effect |
| --- | --- |
| `--output <FILE>` | Destination path for the backup copy |
| `--force` | Overwrite an existing destination file |
