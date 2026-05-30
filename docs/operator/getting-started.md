# Getting started with Clarion

A single-flow walkthrough that takes you from an empty machine to a working
consult-mode agent asking real questions about a real codebase. Target time:
**≤15 minutes** once prerequisites are in place.

You will:

1. [Install Clarion + the Python plugin.](#1-install)
2. [Run `clarion analyze` against a small public Python project.](#2-analyze)
3. [Start `clarion serve` and connect an MCP client.](#3-serve)
4. [Ask three questions through the MCP tools.](#4-ask)
5. [Verify the secret-scanner block fires on a planted secret.](#5-secret-block)

If a step fails, see [Troubleshooting](#troubleshooting) at the end.

## Prerequisites

| Tool | Required version | How to check |
|---|---|---|
| Rust toolchain | `stable` per [`rust-toolchain.toml`](../../rust-toolchain.toml) | `rustc --version` |
| Python | `>= 3.11` per the [plugin manifest](../../plugins/python/pyproject.toml) | `python3 --version` |
| `pipx` (recommended for plugin install) | any recent | `pipx --version` |
| `pyright-langserver` | `1.1.409` — pinned in the [plugin manifest](../../plugins/python/plugin.toml) (`capabilities.runtime.pyright.pin`) | `pyright --version` (the `pyright-langserver` entrypoint only accepts protocol flags like `--stdio`) |
| An MCP client | any MCP-speaking client | see [§3](#3-serve) |

The Python plugin will fail at runtime if `pyright-langserver` is not on
`$PATH` at the pinned version (1.1.409 in v1.0). Install via
`npm install -g pyright@1.1.409` or `pipx install pyright==1.1.409`.

### Required environment variables

For step 4's `summary` question you need an OpenRouter API key:

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
```

`clarion analyze` (step 2) and the structural MCP tools (`entity_at`,
`find_entity`, `callers_of`, `execution_paths_from`, `issues_for`,
`neighborhood`, `subsystem_members`, `subsystem_of`, `project_status`,
`summary_preview_cost`, `source_for_entity`, `call_sites`, `orientation_pack`,
`analyze_start`, `analyze_status`, `analyze_cancel`, `index_diff`) work without
any LLM credentials — seventeen of the eighteen MCP tools are credential-free.
The key is only consulted when an MCP client calls `summary(id)` against an entity that does not
yet have a cached summary.

## 1. Install

Tagged v1.0 releases ship a platform archive for the Rust binary and a Python
sdist for the language plugin via GitHub Releases (per
[ADR-033](../clarion/adr/ADR-033-v1.0-distribution.md)). Until the first tag
fires, use the source-install fallback below.

```bash
TAG=v1.0.0
curl -L -o clarion-x86_64-unknown-linux-gnu.tar.gz \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-x86_64-unknown-linux-gnu.tar.gz"
tar xzf clarion-x86_64-unknown-linux-gnu.tar.gz
install clarion-x86_64-unknown-linux-gnu/clarion ~/.local/bin/

pipx install \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-plugin-python-1.0.0.tar.gz"
```

Source-install fallback:

```bash
# Rust core
cargo install --git https://github.com/tachyon-beep/clarion clarion-cli

# Python plugin (provides clarion-plugin-python on $PATH)
pipx install git+https://github.com/tachyon-beep/clarion#subdirectory=plugins/python
```

Verify the discovery surface:

```bash
which clarion                     # e.g. ~/.cargo/bin/clarion
which clarion-plugin-python       # e.g. ~/.local/bin/clarion-plugin-python
```

### Verifying release artifacts

Tagged releases publish platform archives, SHA256 files, keyless cosign
signatures/certificates, and SLSA provenance. For a downloaded archive:

```bash
sha256sum -c clarion-x86_64-unknown-linux-gnu.tar.gz.sha256
cosign verify-blob \
  --certificate clarion-x86_64-unknown-linux-gnu.tar.gz.pem \
  --signature clarion-x86_64-unknown-linux-gnu.tar.gz.sig \
  --certificate-identity-regexp 'https://github.com/.+/.github/workflows/release.yml@refs/tags/v.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  clarion-x86_64-unknown-linux-gnu.tar.gz
slsa-verifier verify-artifact \
  --provenance-path clarion-rust-binaries.intoto.jsonl \
  --source-uri github.com/tachyon-beep/clarion \
  --source-tag "$TAG" \
  clarion-x86_64-unknown-linux-gnu.tar.gz
```

The v1.0 release deliberately does not publish to PyPI or crates.io. GitHub
Release assets are the source of truth until public registries are introduced
by a later ADR.

**`$PATH` discipline matters.** Clarion's plugin host (per
[ADR-002](../clarion/adr/ADR-002-plugin-transport-json-rpc.md)) discovers
plugins by walking `$PATH` for executables matching `clarion-plugin-*`. If
`pipx`'s install directory (`~/.local/bin/` on Linux, `~/Library/...` on
macOS) is not on your shell's `$PATH`, `clarion analyze` will exit
**successfully** with status `skipped_no_plugins` and emit a `WARN no plugins
discovered` line — the analyse pass produces nothing. See
[Troubleshooting → "analyze runs but emits no entities"](#analyze-runs-but-emits-no-entities)
below for the diagnostic.

## 2. Analyze

Pick a small, well-behaved Python project. The walkthrough uses the `requests`
library's source tree:

```bash
cd /tmp
curl -L -o requests-2.32.4.tar.gz https://github.com/psf/requests/archive/refs/tags/v2.32.4.tar.gz
tar xzf requests-2.32.4.tar.gz
cd requests-2.32.4
```

Initialise Clarion's project-local state, then run the analyser:

```bash
clarion install
clarion analyze
```

Expected output (abridged):

```
applying migration version=1 name="0001_initial_schema"
clarion install complete clarion_dir=/tmp/requests-2.32.4/.clarion
Initialised /tmp/requests-2.32.4/.clarion
...
analyze complete: run <uuid> ok (entities=NNN, edges=MMM)
```

The first run on a tree of this size completes in well under a minute on
typical hardware. The result lives at `.clarion/clarion.db` (a single SQLite
file) and is safe to commit to git — see
[ADR-005](../clarion/adr/ADR-005-clarion-dir-tracking.md).

## 3. Serve

Start the MCP stdio server in one shell:

```bash
clarion serve --path /tmp/requests-2.32.4
```

`clarion serve` speaks the MCP protocol over stdio. Any MCP client works;
documented options:

- **Claude Desktop.** Add to your `claude_desktop_config.json`:

  ```json
  {
    "mcpServers": {
      "clarion-requests": {
        "command": "/path/to/clarion",
        "args": ["serve", "--path", "/tmp/requests-2.32.4"],
        "env": {
          "OPENROUTER_API_KEY": "sk-or-v1-..."
        }
      }
    }
  }
  ```

- **MCP Inspector** (`npm install -g @modelcontextprotocol/inspector`) for
  ad-hoc tool-level exploration without an agent in the loop:

  ```bash
  npx @modelcontextprotocol/inspector clarion serve --path /tmp/requests-2.32.4
  ```

Pick whichever you have; the questions in step 4 are client-agnostic.

### Agent orientation (optional but recommended)

Give consult-mode agents a head start:

```bash
clarion install --skills --path /tmp/requests-2.32.4   # bundle the clarion-workflow skill
clarion install --hooks --path /tmp/requests-2.32.4    # add a SessionStart snapshot hook
clarion install --all   --path /tmp/requests-2.32.4    # .clarion/ init + skills + hooks
```

`--skills` writes `.claude/skills/clarion-workflow/` and `.agents/skills/clarion-workflow/`.
`--hooks` merges a SessionStart entry into `.claude/settings.json` (existing
hooks are preserved) that runs `clarion hook session-start` — a fail-soft
command printing live entity/subsystem/finding counts and index freshness.

Over MCP, the same orientation is available without install: the `initialize`
result carries an `instructions` field, the `clarion://context` resource returns
the live snapshot, and the `clarion-workflow` prompt returns the skill text.

## 4. Ask

### Enable live LLM (one-time)

The structural MCP tools work out of the box, but `summary(id)` (question 3
below) needs the live OpenRouter path explicitly opted into. Edit
`/tmp/requests-2.32.4/clarion.yaml` and set both:

```yaml
llm_policy:
  enabled: true
  allow_live_provider: true
```

`OPENROUTER_API_KEY` must also be exported in the environment that
`clarion serve` (or your MCP client wrapper) inherits — see the
prerequisites section above. Skip this block if you don't have a key; the
other seventeen tools still work, only `summary` will return an "LLM disabled"
envelope.

### The MCP tools

The MCP surface exposes eighteen tools: the seventeen in the table below, plus
`subsystem_members` (the modules in a subsystem — the forward direction of
`subsystem_of`). The table spans entity lookup and navigation
(`entity_at`/`find_entity`/`callers_of`/`execution_paths_from`/`neighborhood`),
clustering (`subsystem_of`), source and edge inspection
(`source_for_entity`/`call_sites`), the one-call orientation packet
(`orientation_pack`), diagnostics (`project_status`/`index_diff`), the
`summary` LLM path plus its `summary_preview_cost` estimator, Filigree
enrichment (`issues_for`), and the background re-index lifecycle
(`analyze_start`/`analyze_status`/`analyze_cancel`). Seventeen of the eighteen
are credential-free; only `summary` needs the live LLM. Each is a structured
graph query, not free-text grep.

| Tool | Example invocation |
|---|---|
| `entity_at(file, line)` | `entity_at(file="requests/sessions.py", line=480)` — which entity covers this source location? |
| `find_entity(pattern)` | `find_entity(pattern="Session.send")` — find entities matching a name or summary fragment. |
| `callers_of(id)` | `callers_of(id="python:function:requests.sessions.Session.send")` — who calls this function? Default confidence is `resolved`. |
| `execution_paths_from(id, max_depth)` | `execution_paths_from(id="python:function:requests.api.get", max_depth=3)` — bounded calls-only paths from an entry point. |
| `summary(id)` | `summary(id="python:function:requests.sessions.Session.send")` — structured LLM summary with `purpose` / `behavior` / `relationships` / `risks` fields. Requires the live-LLM opt-in above plus `OPENROUTER_API_KEY`. First call dispatches the LLM and caches; subsequent calls hit the cache. |
| `issues_for(id)` | `issues_for(id="python:module:requests.sessions")` — Filigree issues attached to this entity, if Filigree is reachable. Returns an `unavailable` envelope if not (Filigree is enrich-only). |
| `neighborhood(id)` | `neighborhood(id="python:function:requests.sessions.Session.send")` — callers, callees, container, contained entities, and references in one hop. |
| `subsystem_of(id)` | `subsystem_of(id="python:module:requests.sessions")` — the subsystem an entity belongs to (reverse of `subsystem_members`); a function/class resolves through its containing module. |
| `project_status()` | `project_status()` — index diagnostics: latest run, entity/edge/finding/briefing-blocked counts, staleness, per-plugin counts, LLM policy, and the resolved Filigree endpoint. No arguments, no LLM. |
| `summary_preview_cost(id)` | `summary_preview_cost(id="python:function:requests.sessions.Session.send")` — preview a `summary` call before spending: cache hit/expired/miss, cached tokens/cost/age, an input-token estimate on a miss, LLM policy, and whether a live call would spend. Never calls the LLM. |
| `source_for_entity(id, context_lines)` | `source_for_entity(id="python:function:requests.sessions.Session.send", context_lines=10)` — the entity's exact indexed source span plus bounded line-numbered context, each line flagged `in_entity`. Reports `source_status` (`ok`/`missing`/`drifted`/…) instead of a stale snippet. No LLM. |
| `call_sites(id, role)` | `call_sites(id="python:function:requests.sessions.Session.send", role="caller")` — the actual source line(s) behind calls/references edges: file, line, line text, edge kind, confidence, and resolved/ambiguous/unresolved classification. `role="callee"` shows incoming sites. No LLM. |
| `orientation_pack(entity \| file, line)` | `orientation_pack(file="requests/sessions.py", line=480)` — one deterministic packet for a location: primary entity, `entity_context` evidence, source-span summary, one-hop neighbors, compact execution paths, related Filigree issues, index/Filigree/LLM health, and suggested next reads. Resolve by `entity` id or by `file`+`line`. No LLM. |
| `analyze_start()` | `analyze_start()` — launch a background `clarion analyze` re-index and return its `run_id` immediately. One run per project (cross-process lock). No arguments, no LLM. |
| `analyze_status(run_id)` | `analyze_status(run_id="…")` — live status of a run: `queued`/`running`/`completed`/`failed`/`cancelled`/`skipped_no_plugins`, phase, processed/total files, heartbeat, and recorded stats on a terminal status. No LLM. |
| `analyze_cancel(run_id)` | `analyze_cancel(run_id="…")` — SIGKILL a running analyze's process group (plugin + Pyright) and record its terminal state. No LLM. |
| `index_diff()` | `index_diff()` — freshness / drift report: latest completed run, indexed-file drift (mtime vs. index), and git working-tree changes correlated against indexed paths. No arguments, no LLM. |

The three questions to walk through with your agent:

1. **"List the top-level modules in this project."** Exercises `find_entity`
   with a broad pattern.
2. **"What calls `requests.get`?"** Exercises `callers_of` against a
   well-known entry point.
3. **"Summarise `requests.sessions.Session.send`."** Exercises the live LLM
   path (`summary`), the OpenRouter provider, the budget ledger, and the
   summary cache. The second invocation of the same `summary(id)` is a cache
   hit; verify by re-asking and noting the near-zero latency.

A successful run gives you three substantive, graph-grounded answers — not
"here is what grep found." If the agent improvises by reading source files
directly, the answer is real but does not exercise the MCP surface; check
that your client actually called the tools.

Re-run analyse for idempotency:

```bash
clarion analyze
# entity/edge counts on the second run should match the first
```

## 5. Secret-block

Plant a fake AWS credential and re-run analyse:

```bash
cat > .env <<'EOF'
AWS_ACCESS_KEY_ID=AKIA0123456789ABCDEF
EOF

clarion analyze
```

Expected behaviour:

- `clarion analyze` exits **0** with run status `completed`.
- A `CLA-SEC-SECRET-DETECTED` finding lands in `findings` with the message
  `AwsAccessKeyId detected in /tmp/requests-2.32.4/.env:1`. Inspect with
  `sqlite3 .clarion/clarion.db "SELECT rule_id, message FROM findings
  WHERE rule_id LIKE 'CLA-SEC%';"`.
- The `.env` file itself has no language entities (it's not Python), so
  the finding is anchored to the core-minted file entity rather than a
  language-plugin entity. Source files in the project that the scanner
  also flags (e.g. high-entropy strings in `requests/utils.py`) get
  `properties.briefing_blocked = "secret_present"` on their containing
  module entity, and the `summary(id)` MCP tool returns a
  `briefing_blocked: "secret_present"` envelope instead of dispatching
  the LLM.

Full mechanics — baseline format, override flags, audit queries — in
[secret-scanning.md](./secret-scanning.md).

## Troubleshooting

### `analyze` runs but emits no entities

Look for `WARN no plugins discovered` and `skipped_no_plugins` in the
analyse output. The plugin host walks `$PATH` for `clarion-plugin-*`
executables; if your shell's `$PATH` does not include `pipx`'s install
directory the plugin is invisible.

Confirm and fix:

```bash
which clarion-plugin-python || echo "not on PATH"
echo $PATH                          # is pipx's bin dir in here?

# If pipx is installed but its bin dir is missing:
pipx ensurepath                     # writes the PATH update; restart shell
```

Note: `clarion analyze` deliberately exits **0** even when no plugins are
discovered, so the run can be re-attempted without manual cleanup. The
`WARN` line and the `skipped_no_plugins` run status are the operator-facing
signals. A `clarion doctor` subcommand that surfaces discovery state at exit
is on the v2.0 roadmap; for v1.0 the diagnostic is the WARN line plus the
`which clarion-plugin-*` check above.

### "secret_present" block fires on a real file

Add the file to `.clarion/secrets-baseline.yaml` with a written justification
(the schema requires it). Full procedure: [secret-scanning.md](./secret-scanning.md).

### `summary` returns an error citing budget or LLM provider

Check `OPENROUTER_API_KEY` is set in the environment that `clarion serve`
inherits (for Claude Desktop that means the `env` block in the MCP-server
config). Live LLM calls are also gated by `llm_policy.enabled: true` and
`llm_policy.allow_live_provider: true` in `clarion.yaml` — see
[openrouter.md](./openrouter.md).

### `issues_for` returns an `unavailable` envelope

Expected when Filigree is not reachable. Filigree integration is
*enrich-only* per the Loom federation axiom — Clarion's structural answers
are unaffected. See
[CON-FILIGREE-02](../clarion/1.0/requirements.md#con-filigree-02--file-registry-displacement-is-deferred-to-v02)
for the v1.0 → v2.0 trajectory.

## Where to go next

- [Operator notes index](./README.md) — OpenRouter, runtime topology,
  secret scanning, federation contracts, coding-agent LLM providers.
- [Design ladder](../clarion/1.0/README.md) — requirements → system-design →
  detailed-design.
- [ADR index](../clarion/adr/README.md) — accepted architecture decisions.
- [CLAUDE.md](../../CLAUDE.md) — repository conventions.
