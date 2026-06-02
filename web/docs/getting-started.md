# Getting Started

This walks you from nothing to a running Clarion MCP server that a consult-mode
agent can query. Everything except `summary(id)` works without any LLM
credentials.

## 1. Install the binary and the Python plugin

Clarion is a single Rust binary; Python support ships as a separate language
plugin. Pull both from the latest GitHub Release:

```bash
TAG=v1.0.0
curl -L -o clarion.tar.gz \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-x86_64-unknown-linux-gnu.tar.gz"
tar xzf clarion.tar.gz
install clarion-x86_64-unknown-linux-gnu/clarion ~/.local/bin/

pipx install \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-plugin-python-1.0.0.tar.gz"
```

Confirm the binary is on your `PATH`:

```bash
clarion --version
```

The Python plugin is discovered on `$PATH` at analyze time. If no plugin is
found, `clarion analyze` exits `0` with a warning and status
`skipped_no_plugins` rather than failing.

## 2. Initialise a project

From inside the repository you want to map:

```bash
cd /path/to/your/python/repo
clarion install --path .
```

`install` creates a project-local `.clarion/` directory (the SQLite store lives
at `.clarion/clarion.db`) and, optionally, installs agent-orientation assets:

- `clarion install --skills` — drop the bundled `clarion-workflow` skill pack
  into `.claude/skills/` and `.agents/skills/`.
- `clarion install --hooks` — merge a `SessionStart` hook into
  `.claude/settings.json`.
- `clarion install --all` — do everything (equivalent to a bare `install`).

!!! note "Where state lives"
    `clarion analyze` always persists to the project root's `.clarion/`, not to
    wherever `--path` pointed at install time. To re-index a corpus cleanly,
    remove the stale `.clarion/` first (or pass `--no-incremental`).

## 3. Build the graph

```bash
clarion analyze
```

`analyze` walks the source tree, dispatches the discovered language plugin to
extract entities and edges, and persists the result. Re-runs are idempotent
(UPSERT on the entity id) and incremental by default — unchanged files are
skipped. Pass `--no-incremental` to force a clean re-index.

This step needs **no LLM credentials**. It is the fastest way to verify the
install end-to-end.

## 4. Serve the graph over MCP

```bash
clarion serve
```

This starts the MCP stdio server. Point your MCP client at it — for Claude
Code, register it in `.mcp.json`:

```json
{
  "mcpServers": {
    "clarion": {
      "command": "clarion",
      "args": ["serve", "--path", "."]
    }
  }
}
```

Your agent can now call Clarion's consult tools instead of re-exploring the
tree. See [MCP consult tools](concepts/mcp-tools.md) for the workflow and
[the MCP tool reference](reference/mcp-tools.md) for the core tools.

## 5. (Optional) Enable on-demand summaries

`summary(id)` dispatches an LLM lazily — one entity at a time, cached after the
first call. Set `OPENROUTER_API_KEY` in the environment (or a project `.env`)
before calling it. Structural extraction never needs this; summarisation is the
only path that makes network calls to a model provider.

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `analyze` exits with `skipped_no_plugins` | No language plugin on `$PATH` | `pipx install` the Python plugin (step 1) |
| Stale entities after a big refactor | Incremental skip kept old rows | `clarion analyze --no-incremental` |
| Agent can't reach the server | MCP registration missing | `clarion doctor --fix` |

`clarion doctor` verifies the skill pack, the `SessionStart` hook, and the
`.mcp.json` registration, and repairs them in place with `--fix`.
