# Getting started with Loomweave

A single-flow walkthrough that takes you from an empty machine to a working
consult-mode agent asking real questions about a real codebase. Target time:
**≤15 minutes** once prerequisites are in place.

You will:

1. [Install Loomweave + the Python plugin.](#1-install)
2. [Run `loomweave analyze` against a small public Python project.](#2-analyze)
3. [Start `loomweave serve` and connect an MCP client.](#3-serve)
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
`$PATH` at the pinned version (currently 1.1.409). Install via
`npm install -g pyright@1.1.409` or `pipx install pyright==1.1.409`.

### Required environment variables

For step 4's `entity_summary_get` question you need an OpenRouter API key:

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
```

`loomweave analyze` (step 2) and the structural MCP tools work without any LLM
credentials. The key is only consulted when an MCP client calls
`entity_summary_get(id)` against an entity that does not yet have a cached
summary — and only once `llm_policy.enabled: true` **and**
`llm_policy.allow_live_provider: true` are set in `loomweave.yaml`
(`loomweave install` seeds the file with both `false`; see [§4](#4-ask)).

## 1. Install

Tagged releases ship a platform archive for the Rust binary and a Python sdist
for the language plugin via GitHub Releases (per
[ADR-033](../loomweave/adr/ADR-033-v1.0-distribution.md)). Use the source-install
fallback below only when testing unreleased commits.

```bash
TAG=v1.6.1
curl -L -o loomweave-x86_64-unknown-linux-gnu.tar.gz \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-x86_64-unknown-linux-gnu.tar.gz"
tar xzf loomweave-x86_64-unknown-linux-gnu.tar.gz
install loomweave-x86_64-unknown-linux-gnu/loomweave ~/.local/bin/

pipx install \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave_plugin_python-1.6.1.tar.gz"
```

Source-install fallback:

```bash
# Rust core
cargo install --git https://github.com/foundryside-dev/loomweave loomweave-cli

# Python plugin (provides loomweave-plugin-python on $PATH)
pipx install git+https://github.com/foundryside-dev/loomweave#subdirectory=plugins/python
```

Verify the discovery surface:

```bash
which loomweave                     # e.g. ~/.cargo/bin/loomweave
which loomweave-plugin-python       # e.g. ~/.local/bin/loomweave-plugin-python
```

Rust source is analysed by a separate first-party plugin: the
`loomweave-plugin-rust` wheel/binary, shipped and installed independently of the
Python plugin. It is discovered the same way — the host walks `$PATH` for
`loomweave-plugin-*` executables (see the [`$PATH` discipline](#path-discipline)
paragraph below), so `loomweave-plugin-rust` only needs to be on `$PATH` to be
picked up. Install it when you intend to analyse a Rust tree; this walkthrough
targets a Python project and uses only the Python plugin. See
[Rust analysis: known limitations](./rust-known-limitations.md).

### Verifying release artifacts

Tagged releases publish platform archives, SHA256 files, keyless cosign
signatures/certificates, and SLSA provenance. For a downloaded archive:

```bash
sha256sum -c loomweave-x86_64-unknown-linux-gnu.tar.gz.sha256
cosign verify-blob \
  --certificate loomweave-x86_64-unknown-linux-gnu.tar.gz.pem \
  --signature loomweave-x86_64-unknown-linux-gnu.tar.gz.sig \
  --certificate-identity-regexp 'https://github.com/.+/.github/workflows/release.yml@refs/tags/v.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  loomweave-x86_64-unknown-linux-gnu.tar.gz
slsa-verifier verify-artifact \
  --provenance-path loomweave-rust-binaries.intoto.jsonl \
  --source-uri github.com/foundryside-dev/loomweave \
  --source-tag "$TAG" \
  loomweave-x86_64-unknown-linux-gnu.tar.gz
```

Tagged releases also publish to PyPI and crates.io, but the GitHub Release
assets remain the source of truth for this walkthrough.

<a id="path-discipline"></a>
**`$PATH` discipline matters.** Loomweave's plugin host (per
[ADR-002](../loomweave/adr/ADR-002-plugin-transport-json-rpc.md)) discovers
plugins by walking `$PATH` for executables matching `loomweave-plugin-*`
(this is the generic mechanism for every language plugin, Python and Rust
alike). If
`pipx`'s install directory (`~/.local/bin/` on Linux, `~/Library/...` on
macOS) is not on your shell's `$PATH`, `loomweave analyze` will exit
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

Initialise Loomweave's project-local state, then run the analyser:

```bash
loomweave install
loomweave analyze
```

A bare `loomweave install` does everything: it initialises `.weft/loomweave/`, installs
the agent-orientation assets, writes Claude Code MCP config, and upserts the
Codex MCP config (see [§3](#agent-orientation-installed-by-default)). If
`.weft/loomweave/` already exists, init is skipped and the other components are applied
idempotently; pass `--force` to wipe and reinitialise the index.

Expected output (abridged):

```
applying migration version=1 name="0001_initial_schema"
loomweave install complete loomweave_dir=/tmp/requests-2.32.4/.weft/loomweave
Initialised /tmp/requests-2.32.4/.weft/loomweave
Installed loomweave-workflow skill into ...
Installed Claude Code MCP config at .../.mcp.json
Installed Codex MCP config at ~/.codex/config.toml
Added loomweave SessionStart hook to .../.claude/settings.json
...
analyze complete: run <uuid> ok (entities=NNN, edges=MMM)
```

The first run on a tree of this size completes in well under a minute on
typical hardware. The result lives at `.weft/loomweave/loomweave.db` (a single SQLite
file) and must **not** be committed to git: it is a regenerable index that
churns on every analyze, and committing it dirties the tree on every run. The
store's own `.gitignore` (written by `loomweave install`) already covers it, so
in a normal install there is nothing to do. If an older layout or a `git add -f`
put it in the index anyway, `loomweave doctor`'s `db.tracked` check reports it
and `loomweave doctor --fix` untracks it. (ADR-005 originally said the opposite;
it was reversed — the `db.tracked` gate is the current rule.)

For full `tests/` → `src/` call resolution, give the project a `.venv` before
analyzing (or set `LOOMWEAVE_PYTHON_INTERPRETER` to its interpreter) —
Pyright resolves calls against whatever interpreter it finds, and an
unpinned interpreter silently misses cross-module targets. A `.venv` that is
tracked by the repository itself is skipped on this rung (pyright would
otherwise execute it as `python.pythonPath`), so keep `.venv` out of version
control. (The `requests` tarball above has no `.venv`, so this walkthrough's
own call resolution is correspondingly partial.) See
[ADR-058](../loomweave/adr/ADR-058-project-interpreter-discovery.md).

## 3. Serve

Start the MCP stdio server in one shell:

```bash
loomweave serve --path /tmp/requests-2.32.4
```

`loomweave serve` speaks the MCP protocol over stdio. Any MCP client works;
documented options:

- **Claude Desktop.** Add to your `claude_desktop_config.json`:

  ```json
  {
    "mcpServers": {
      "loomweave-requests": {
        "command": "/path/to/loomweave",
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
  npx @modelcontextprotocol/inspector loomweave serve --path /tmp/requests-2.32.4
  ```

Pick whichever you have; the questions in step 4 are client-agnostic.

### Agent orientation (installed by default)

A bare `loomweave install` already bundles these for consult-mode agents. The
component flags exist for explicit partial installs (e.g. adding the skill to a
project whose `.weft/loomweave/` you do not want re-touched):

```bash
loomweave install --claude-code --path /tmp/requests-2.32.4  # Claude Code MCP only
loomweave install --codex --path /tmp/requests-2.32.4        # Codex MCP only
loomweave install --skills --path /tmp/requests-2.32.4       # Claude skill only
loomweave install --codex-skills --path /tmp/requests-2.32.4 # Codex skill only
loomweave install --hooks --path /tmp/requests-2.32.4        # hook only
loomweave install --all --path /tmp/requests-2.32.4          # same as bare install
```

`--skills` writes `.claude/skills/loomweave-workflow/`; `--codex-skills` writes
`.agents/skills/loomweave-workflow/`. `--claude-code` writes `.mcp.json` with a
stdio `loomweave serve` entry. `--codex` upserts `[mcp_servers.loomweave]` in
`~/.codex/config.toml`. Both MCP configs rely on the client working directory
for project discovery instead of pinning `--path`.
`--hooks` merges a SessionStart entry into `.claude/settings.json` (existing
hooks are preserved) that runs `loomweave hook session-start` — a fail-soft
command printing live entity/subsystem/finding counts and index freshness —
and a Loomweave-managed block into the repository's `post-merge` and
`post-checkout` git hooks (branch switches only) that runs
`loomweave hook git-sync`, a detached background refresh when the index is
stale. Both are fail-soft and never block git or a session. A refresh that
finds another analyze running is queued and drained when that one finishes;
a commit that touches nothing Loomweave indexes (docs, CI config) settles in
about a second on the analyze fast path instead of re-scanning the tree.
`post-commit` is deliberately not hooked — a commit changes no file content
the index has not already seen; the index refreshes on merge, branch switch,
session start, and on demand (`analyze_start`).

To verify (and repair) these surfaces later, run `loomweave doctor`:

```bash
loomweave doctor --path /tmp/requests-2.32.4          # report only; exits non-zero if anything is off
loomweave doctor --fix --path /tmp/requests-2.32.4    # repair the skill pack, hook, and .mcp.json entry in place
loomweave doctor --format json --path /tmp/requests-2.32.4  # stable check IDs + machine-readable details
```

`doctor` also checks the `loomweave` entry in `.mcp.json` that a bare `install`
(or `install --claude-code`) registers, and `--fix` re-adds it if it has gone
missing (preserving any sibling MCP servers and a customised `command`). The non-zero exit on remaining problems
makes it usable as a CI / pre-commit gate.

The federation diagnostics use the same fail-closed readers as the serving
surfaces. `federation.sqlite_compatibility` reports the exact application ID,
user version, and external-read compatibility without migrating the database.
`classifier.enumeration` reports latest-run discovery/source-walk completeness,
while `classifier.tags` separately lists declarations only for plugins that
actually matched source files (a `not-applicable` plugin is not active).
`http.authentication` reports the effective `none`, `bearer`, or `hmac` posture
without exposing secret values, and `http.instance_id` validates the persisted
project UUID. A failed latest run, malformed coverage/config/instance ID, or an
incompatible database is a gating problem; an absent optional index and an
accepted legacy/older external schema are advisory. All database diagnostics,
including `sei.population`, open the catalogue read-only: running doctor never
creates a missing `loomweave.db`.

Over MCP, the same orientation is available without install: the `initialize`
result carries an `instructions` field, the `loomweave://context` resource returns
the live snapshot, and the `loomweave-workflow` prompt returns `SKILL.md` — the
skill's entry point. Its depth lives in `references/*.md` alongside the installed
skill (relocated there under weft convention C-20); the prompt does not inline
them, so `--skills` / `--codex-skills` is what puts the full reference on disk.

## 4. Ask

### Enable live LLM (one-time)

The structural MCP tools work out of the box, but `entity_summary_get(id)`
(question 3 below) needs the live OpenRouter path explicitly opted into. Edit
`/tmp/requests-2.32.4/loomweave.yaml` and set both:

```yaml
llm_policy:
  enabled: true
  allow_live_provider: true
```

`OPENROUTER_API_KEY` must also be exported in the environment that
`loomweave serve` (or your MCP client wrapper) inherits — see the
prerequisites section above. Skip this block if you don't have a key; the
credential-free tools still work, and the LLM-backed paths —
`entity_summary_get`, `entity_summary_preview_cost_get`, graph queries asked
for `confidence: "inferred"`, and `entity_semantic_search_list` against a
hosted embedding provider — return an "LLM disabled" envelope instead.

Run `loomweave config check` after editing to confirm the effective state
(provider, enabled, live, model) before starting `serve` — it flags the common
mistakes (a provider left `enabled: false`, a missing key, or a misplaced key,
which is now a hard parse error rather than a silent drop).

### Who owns `loomweave.yaml`

`loomweave.yaml` is **operator-local**, not project state. Keep it out of
version control — the first thing to do after `loomweave install` in a
repository is:

```bash
echo loomweave.yaml >> .gitignore
```

Loomweave enforces the same boundary from its side
([ADR-063](../loomweave/adr/ADR-063-repository-content-is-not-operator-intent.md)):
when the effective `loomweave.yaml` is **tracked by the repository that
contains it**, it is treated as repository content rather than as your
configuration, and its egress-capable sections are replaced with their
defaults before anything reads them — `llm_policy`, `semantic_search`,
`integrations` and `serve.http`. A tracked config can still shape *analysis*:
`version`, the `analysis:` clustering block, and `serve.mcp` are honoured. What
it cannot do is name a network endpoint, name the environment variable whose
value is sent as a credential, or open a listener — which is exactly what a
`loomweave.yaml` committed by someone else's repository would otherwise get to
choose the moment you opened their checkout.

If your provider is unexpectedly disabled, this is the first thing to check.
Every surface reports the verdict: `loomweave config check` prints a
`config trust:` line, `loomweave doctor` runs a `config.trust` check, `serve`
and `analyze` log it once at startup, and the `project_status_get` /
`llm_config_get` MCP tools carry a `config_trust` field. The fix is to take the
file back:

```bash
git rm --cached loomweave.yaml && echo loomweave.yaml >> .gitignore
```

`loomweave doctor --fix` does this for you (it will not edit a `.gitignore`
that the repository tracks — it untracks the config and tells you to add the
line yourself).

### The MCP tools

The MCP surface exposes 48 tools. The eighteen core ones are the seventeen in
the table below, plus `subsystem_member_list` (the modules in a subsystem —
the forward direction of `entity_subsystem_get`); the remaining thirty are
faceted, inspection, and shortcut queries (`entity_tag_list`,
`entity_dead_list`, `entity_semantic_search_list`, …) listed in the
[README](../../README.md#what-it-does-today) and the `loomweave-workflow`
skill. The table spans entity lookup and navigation
(`entity_at`/`entity_find`/`entity_callers_list`/`entity_execution_path_list`/`entity_neighborhood_get`),
clustering (`entity_subsystem_get`), source and edge inspection
(`entity_source_get`/`entity_call_site_list`), the one-call orientation packet
(`entity_orientation_pack_get`), diagnostics (`project_status_get`/`index_diff_get`),
the `entity_summary_get` LLM path plus its `entity_summary_preview_cost_get`
estimator, Filigree enrichment (`entity_issue_list`), and the background
re-index lifecycle (`analyze_start`/`analyze_status_get`/`analyze_cancel`).
Of these, only `entity_summary_get` needs the live LLM; the graph queries are
credential-free at their default confidence (asking for
`confidence: "inferred"` routes through the LLM and, without the opt-in,
returns an "LLM disabled" envelope). Each is a structured graph query, not
free-text grep.

| Tool | Example invocation |
|---|---|
| `entity_at(file, line)` | `entity_at(file="requests/sessions.py", line=480)` — which entity covers this source location? |
| `entity_find(pattern)` | `entity_find(pattern="Session.send")` — find entities matching a name or summary fragment. |
| `entity_callers_list(id)` | `entity_callers_list(id="python:function:requests.sessions.Session.send")` — who calls this function? Default confidence is `resolved`. |
| `entity_execution_path_list(id, max_depth)` | `entity_execution_path_list(id="python:function:requests.api.get", max_depth=3)` — bounded calls-only paths from an entry point. |
| `entity_summary_get(id)` | `entity_summary_get(id="python:function:requests.sessions.Session.send")` — structured LLM summary with `purpose` / `behavior` / `relationships` / `risks` fields. Requires the live-LLM opt-in above plus `OPENROUTER_API_KEY`. First call dispatches the LLM and caches; subsequent calls hit the cache. |
| `entity_issue_list(id)` | `entity_issue_list(id="python:module:requests.sessions")` — Filigree issues attached to this entity, if Filigree is reachable. Returns an `unavailable` envelope if not (Filigree is enrich-only). |
| `entity_neighborhood_get(id)` | `entity_neighborhood_get(id="python:function:requests.sessions.Session.send")` — callers, callees, container, contained entities, and references in one hop. |
| `entity_subsystem_get(id)` | `entity_subsystem_get(id="python:module:requests.sessions")` — the subsystem an entity belongs to (reverse of `subsystem_member_list`); a function/class resolves through its containing module. |
| `project_status_get()` | `project_status_get()` — index diagnostics: latest run, entity/edge/finding/briefing-blocked counts, staleness, per-plugin counts, LLM policy, and the resolved Filigree endpoint. No arguments, no LLM. |
| `entity_summary_preview_cost_get(id)` | `entity_summary_preview_cost_get(id="python:function:requests.sessions.Session.send")` — preview an `entity_summary_get` call before spending: cache hit/expired/miss, cached tokens/cost/age, an input-token estimate on a miss, LLM policy, and whether a live call would spend. Never calls the LLM. |
| `entity_source_get(id, context_lines)` | `entity_source_get(id="python:function:requests.sessions.Session.send", context_lines=10)` — the entity's exact indexed source span plus bounded line-numbered context, each line flagged `in_entity`. Reports `source_status` (`ok`/`missing`/`drifted`/…) instead of a stale snippet. No LLM. |
| `entity_call_site_list(id, role)` | `entity_call_site_list(id="python:function:requests.sessions.Session.send", role="caller")` — the actual source line(s) behind calls/references edges: file, line, line text, edge kind, confidence, and resolved/ambiguous/unresolved classification. `role="callee"` shows incoming sites. No LLM. |
| `entity_orientation_pack_get(entity \| file, line)` | `entity_orientation_pack_get(file="requests/sessions.py", line=480)` — one deterministic packet for a location: primary entity, `entity_context` evidence, source-span summary, one-hop neighbors, compact execution paths, related Filigree issues, index/Filigree/LLM health, and suggested next reads. Resolve by `entity` id or by `file`+`line`. No LLM. |
| `analyze_start()` | `analyze_start()` — launch a background `loomweave analyze` re-index and return its `run_id` immediately. One run per project (cross-process lock). No arguments, no LLM. |
| `analyze_status_get(run_id)` | `analyze_status_get(run_id="…")` — live status of a run: `queued`/`running`/`completed`/`failed`/`cancelled`/`skipped_no_plugins`, phase, processed/total files, heartbeat, and recorded stats on a terminal status. No LLM. |
| `analyze_cancel(run_id)` | `analyze_cancel(run_id="…")` — SIGKILL a running analyze's process group (plugin + Pyright) and record its terminal state. No LLM. |
| `index_diff_get()` | `index_diff_get()` — freshness / drift report: latest completed run, indexed-file drift (mtime vs. index), and git working-tree changes correlated against indexed paths. No arguments, no LLM. |

The three questions to walk through with your agent:

1. **"List the top-level modules in this project."** Exercises `entity_find`
   with a broad pattern.
2. **"What calls `requests.get`?"** Exercises `entity_callers_list` against a
   well-known entry point.
3. **"Summarise `requests.sessions.Session.send`."** Exercises the live LLM
   path (`entity_summary_get`), the OpenRouter provider, the budget ledger, and
   the summary cache. The second invocation of the same
   `entity_summary_get(id)` is a cache
   hit; verify by re-asking and noting the near-zero latency.

A successful run gives you three substantive, graph-grounded answers — not
"here is what grep found." If the agent improvises by reading source files
directly, the answer is real but does not exercise the MCP surface; check
that your client actually called the tools.

Re-run analyse for idempotency:

```bash
loomweave analyze
# entity/edge counts on the second run should match the first
```

## 5. Secret-block

Plant a fake AWS credential and re-run analyse:

```bash
cat > .env <<'EOF'
AWS_ACCESS_KEY_ID=AKIA0123456789ABCDEF
EOF

loomweave analyze
```

Expected behaviour:

- `loomweave analyze` exits **0** with run status `completed`.
- A `LMWV-SEC-SECRET-DETECTED` finding lands in `findings` with the message
  `AwsAccessKeyId detected in /tmp/requests-2.32.4/.env:1`. Inspect with
  `sqlite3 .weft/loomweave/loomweave.db "SELECT rule_id, message FROM findings
  WHERE rule_id LIKE 'LMWV-SEC%';"`.
- The `.env` file itself has no language entities (it's not Python), so
  the finding is anchored to the core-minted file entity rather than a
  language-plugin entity. Source files in the project that the scanner
  also flags (e.g. high-entropy strings in `requests/utils.py`) get
  `properties.briefing_blocked = "secret_present"` on their containing
  module entity, and the `entity_summary_get(id)` MCP tool returns a
  `briefing_blocked: "secret_present"` envelope instead of dispatching
  the LLM.

Full mechanics — baseline format, override flags, audit queries — in
[secret-scanning.md](./secret-scanning.md).

## Troubleshooting

### `analyze` runs but emits no entities

Look for `WARN no plugins discovered` and `skipped_no_plugins` in the
analyse output. The plugin host walks `$PATH` for `loomweave-plugin-*`
executables; if your shell's `$PATH` does not include `pipx`'s install
directory the plugin is invisible.

Confirm and fix:

```bash
which loomweave-plugin-python || echo "not on PATH"
echo $PATH                          # is pipx's bin dir in here?

# If pipx is installed but its bin dir is missing:
pipx ensurepath                     # writes the PATH update; restart shell
```

Note: `loomweave analyze` deliberately exits **0** even when no plugins are
discovered, so the run can be re-attempted without manual cleanup. The
`WARN` line and the `skipped_no_plugins` run status are the operator-facing
signals. `loomweave doctor` (see [§3](#agent-orientation-installed-by-default))
also surfaces plugin discovery state, reporting per-plugin presence for both the
Python and Rust plugins; the WARN line plus the `which loomweave-plugin-*` check
above remain the quickest manual diagnostic.

### macOS: "loomweave cannot be opened because the developer cannot be verified"

The release archives are not notarized (ADR-033 ships unsigned binaries), so
macOS Gatekeeper quarantines the downloaded `loomweave` binary and refuses the
first launch with a developer-verification error. Clear the quarantine
attribute on the extracted binary before installing it:

```bash
xattr -d com.apple.quarantine ./loomweave-aarch64-apple-darwin/loomweave
```

Alternatively, approve it once from the GUI — attempt to run it, then
**System Settings → Privacy & Security → "Open Anyway"**. Either is a one-time
step per downloaded binary; a source build (the fallback under [§1](#1-install))
is never quarantined. Notarized release artifacts are on the post-1.0 roadmap.

### "secret_present" block fires on a real file

Add the file to `.weft/loomweave/secrets-baseline.yaml` with a written justification
(the schema requires it). Full procedure: [secret-scanning.md](./secret-scanning.md).

### `entity_summary_get` returns an error citing budget or LLM provider

Check `OPENROUTER_API_KEY` is set in the environment that `loomweave serve`
inherits (for Claude Desktop that means the `env` block in the MCP-server
config). Live LLM calls are also gated by `llm_policy.enabled: true` and
`llm_policy.allow_live_provider: true` in `loomweave.yaml` — see
[openrouter.md](./openrouter.md).

### `entity_issue_list` returns an `unavailable` envelope

Expected when Filigree is not reachable. Filigree integration is
*enrich-only* per the Weft federation axiom — Loomweave's structural answers
are unaffected. See
[CON-FILIGREE-02](../loomweave/1.0/requirements.md#con-filigree-02--file-registry-displacement-is-deferred-to-v02)
for the v1.0 → v2.0 trajectory.

## Where to go next

- [Language support](./language-support.md) — what each language plugin (Python,
  Rust) extracts and tags, side by side: entity/edge kinds, categorisation tags,
  and which tools (e.g. dead-code) work per language.
- [Operator notes index](./README.md) — OpenRouter, runtime topology,
  secret scanning, federation contracts, coding-agent LLM providers.
- [Design ladder](../loomweave/1.0/README.md) — requirements → system-design →
  detailed-design.
- [ADR index](../loomweave/adr/README.md) — accepted architecture decisions.
- [CLAUDE.md](../../CLAUDE.md) — repository conventions.
