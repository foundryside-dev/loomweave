---
template: home.html
hide:
  - navigation
  - toc
---

## Install

Loomweave is a single Rust binary plus a Python language plugin. Grab both from
the latest GitHub Release:

```bash
TAG=v1.2.0
curl -L -o loomweave.tar.gz \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-x86_64-unknown-linux-gnu.tar.gz"
tar xzf loomweave.tar.gz
install loomweave-x86_64-unknown-linux-gnu/loomweave ~/.local/bin/
pipx install \
  "https://github.com/foundryside-dev/loomweave/releases/download/${TAG}/loomweave-plugin-python-1.2.0.tar.gz"
```

The [Getting Started](getting-started.md) guide covers a fresh-machine install,
running against a real Python project, and connecting an MCP client.

## 30-second example

Point Loomweave at a Python repo, build the graph, and serve it:

```bash
cd /path/to/your/python/repo
loomweave install --path .   # initialise the project's .loomweave/ store
loomweave analyze            # walk the corpus, persist entities + edges
loomweave serve              # expose the graph to your agent over MCP
```

`loomweave analyze` runs with **no LLM credentials** and is the fastest way to
verify the install — it walks the corpus and writes the structural graph. Only
`summary(id)` calls dispatch the LLM, lazily and one entity at a time.

Once `loomweave serve` is running, a consult-mode agent reaches a graph-aware tool
instead of grep-and-read. Ask which entity covers a source location, then expand
its one-hop neighborhood:

```json
{
  "entity_id": "python:function:auth.tokens.refresh",
  "callers": ["api.session.login", "api.session.renew"],
  "callees": ["auth.tokens._mint", "http.client.post"],
  "subsystem": "auth",
  "scope_excludes": ["attribute-receiver-calls"]
}
```

That is one `neighborhood(id)` response: callers, callees, container, and a
`scope_excludes` block that names the static blind spots not searched — so an
empty section is never mistaken for a guaranteed true negative.

## Next steps

- [Getting Started](getting-started.md) — install, analyze a repo, connect an agent.
- [The entity model](concepts/entity-model.md) — entity IDs, kinds, and the edge graph.
- [MCP consult tools](concepts/mcp-tools.md) — how an agent uses Loomweave instead of re-exploring.
