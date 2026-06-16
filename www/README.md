# Loomweave — product site

Static front door for **Loomweave**, the Rust code-archaeology tool and the
Weft Federation's SEI identity authority. A faithful application of the
**Weft Design System** to a multi-page product site — terminal-grade,
warm-espresso "Loom" theme, JetBrains Mono as the product face with Space
Grotesk reserved for brand moments. Hand-rolled HTML/CSS/JS, **no build step, no
runtime dependencies**. GitHub-Pages-deployable as-is at `loomweave.foundryside.dev`.

## Files

| File | Purpose |
|---|---|
| `index.html` | Landing page: hero (defining line + SEI axiom + 4-stat strip), "what Loomweave owns", 30-second quick start (with the `scope_excludes` honesty note), the federation role section, and cards into the other pages. |
| `getting-started.html` | install → init → analyze → serve → optional summaries, the `.mcp.json` wiring, and a troubleshooting table. |
| `concepts.html` | The entity model (locator vs. SEI identity), kinds, the edge graph, subsystems, the consult loop, `scope_excludes`, and enrich-only design. |
| `tools.html` | The ~42-tool MCP consult surface, grouped by family. |
| `cli.html` | The `loomweave` command set with every flag. |
| `colors_and_type.css` | **Token + type source of truth — copied VERBATIM from the design system. Never edit it here** (see below). |
| `styles.css` | Site layout + components, layered on the tokens. Carries the single accent remap and all net-new vocabulary. |
| `main.js` | Progressive enhancement only: copy-to-clipboard on code blocks. The site is fully content-complete with JS off. |
| `fonts/` | JetBrains Mono (upright + italic) and Space Grotesk variable TTFs + OFL licenses. Bundled locally — fully offline, no CDN. |
| `assets/marks/` | The federation glyph set as standalone SVGs. The Loomweave 3-spoke node mark is also inlined in each page so it inherits `currentColor`. |
| `CNAME` | `loomweave.foundryside.dev` (GitHub Pages custom domain). |
| `.nojekyll` | Serve files verbatim on GitHub Pages (no Jekyll processing). |

## Preview locally

```bash
python3 -m http.server 8000
```

Then open `http://localhost:8000/`. Use `localhost` (not `file://`) so the
preloaded fonts resolve under a normal origin.

## The verbatim-token-copy discipline

`colors_and_type.css` and `fonts/` are **copied unchanged** from the Weft Design
System (`~/weft/www/`). They are the suite-wide token + type source of truth.

- **Never edit `colors_and_type.css` here.** On a design-system update, **re-copy**
  it from the source rather than hand-patching it.
- All Loomweave-specific styling lives in `styles.css`, layered *after* the token
  file so it overrides cleanly.

## Deliberate decisions

- **Aqua accent remap.** This is the Loomweave *product* site, so the shared
  interactive accent is remapped from the suite amber to Loomweave's own thread,
  **aqua `#52C9B8`** ("structure + identity spine") — exactly as a sibling product
  site remaps the shared ramp to its own thread. The remap is a small `:root`
  override at the top of `styles.css` (`--accent`, `--accent-hover`,
  `--accent-subtle`, `--text-on-accent`, `--focus-ring`, `--glow-accent`).
  **Everything else is untouched**: surfaces, the text ramp, the *other* thread
  colors (so siblings keep their colors in the federation section), radii,
  spacing, and the type scale stay exactly as the token file defines.
- **Dark only.** Warm espresso is the canonical "Loom" theme; the design system
  ships no toggle, so none is added here. `data-theme="dark"` is set on `<html>`.
  The light theme lives in the tokens under `[data-theme="light"]` if wanted later.
- **Loomweave glyph, not the Weft mark.** The header/footer use Loomweave's own
  3-spoke node glyph (from `assets/marks/loomweave.svg`), not the hub's woven
  Weft mark.
- **Reduced motion honored**, and the page is content-complete with JS disabled.

## Facts corrected from the repo

The repo is the authority. Where the older mkdocs source this site replaces was
stale, it was corrected:

- **Repo:** `github.com/foundryside-dev/loomweave` (the `clarion` rename has
  landed; the old "clarion" notes were dropped).
- **Store path:** `.weft/loomweave/` (e.g. `.weft/loomweave/loomweave.db`) — the
  old docs said `.loomweave/`.
- **Version line:** normalised to `1.1.0` (`TAG=v1.1.0`, wheel
  `loomweave-plugin-python-1.1.0.tar.gz`) — the old docs drifted between v1.2.0
  and v1.0.0.
- **MCP surface:** described as "~40 consult-mode MCP tools".
- **MCP tool names:** the live `entity_*` names (e.g. `entity_callers_list`,
  `entity_neighborhood_get`, `entity_orientation_pack_get`), not the older
  shorthand (`callers_of`, `neighborhood`).
- **Scope:** Rust core + Python language plugin; other languages are v2.0+ scope.
- **Local-first:** no mandatory cloud; only network egress is the LLM provider
  during `summary` calls; `analyze` needs no credentials. Provider is OpenRouter
  or a local Claude/Codex CLI.
- **License:** MIT.

## Integration with the Weft hub

Header nav and footer link back to the Weft Federation hub
(`github.com/foundryside-dev/weft` — the hub has no custom domain) and carry the
Foundryside attribution (`foundryside.dev`), mirroring the hub footer. The
footer note is in the Loomweave voice — Loomweave *is* installable and runnable,
unlike the documentation-only hub.

The reverse direction (rewiring the hub's Loomweave card to point here) lives in
the `~/weft` repo and is handled separately.
