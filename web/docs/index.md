---
template: home.html
hide:
  - navigation
  - toc
---

## Install

Clarion is a single Rust binary plus a Python language plugin. Grab both from
the latest GitHub Release:

```bash
TAG=v1.2.0
curl -L -o clarion.tar.gz \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-x86_64-unknown-linux-gnu.tar.gz"
tar xzf clarion.tar.gz
install clarion-x86_64-unknown-linux-gnu/clarion ~/.local/bin/
pipx install \
  "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-plugin-python-1.2.0.tar.gz"
```

The [Getting Started](getting-started.md) guide covers a fresh-machine install,
running against a real Python project, and connecting an MCP client.

## 30-second example

Point Clarion at a Python repo, build the graph, and serve it:

```bash
cd /path/to/your/python/repo
clarion install --path .   # initialise the project's .clarion/ store
clarion analyze            # walk the corpus, persist entities + edges
clarion serve              # expose the graph to your agent over MCP
```

`clarion analyze` runs with **no LLM credentials** and is the fastest way to
verify the install — it walks the corpus and writes the structural graph. Only
`summary(id)` calls dispatch the LLM, lazily and one entity at a time.

Once `clarion serve` is running, a consult-mode agent reaches a graph-aware tool
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
- [MCP consult tools](concepts/mcp-tools.md) — how an agent uses Clarion instead of re-exploring.
