# Loomweave — Product Vision

> Bootstrapped 2026-06-11 from observed reality (README.md, docs/loomweave/1.0/,
> git history, Filigree tracker). Items marked **[ASSUMED]** were inferred, not
> found verbatim — confirm or correct them.

## Purpose

Loomweave is a code-archaeology tool: it pre-extracts a codebase into a
queryable structural graph (entities, call/reference/import/relation edges,
subsystem clusters, stable entity identity) and serves it to consult-mode LLM
agents over MCP — so an agent asks a graph-aware tool instead of re-grepping
the tree on every question.

## Who it serves

1. **Consult-mode LLM agents** (Claude Code, Codex, any MCP client) — the
   primary consumer of the ~42-tool surface.
2. **Operators of those agents** — developers who run `loomweave install /
   analyze / serve` on their repos and author guidance sheets.
3. **Weft suite siblings** (Filigree issue tracker, Wardline trust tooling) —
   consume the federation HTTP read API and SEI tokens. Enrichment consumers,
   never dependents.

## Anti-goals

- **No mandatory cloud component.** Local-first; the only required network
  egress is the LLM provider during opt-in `summary` calls. `analyze` runs
  with no credentials.
- **No hard federation dependencies.** Filigree/Wardline integration is
  enrichment; everything degrades cleanly when a sibling is absent.
- **Not a linter, security scanner, or CI gate.** Findings exist
  (secret scanner, degraded-file findings) but the product is a *map*, not a
  judge. **[ASSUMED]**
- **No eager LLM spend.** Summaries are lazy, per-entity, on-demand only.
- **Not all-languages-now.** v1.x is Python + Rust plugins; Java/TypeScript
  are v2.0+ scope (NG-15).

## North star **[ASSUMED]**

A consult agent working in an indexed repo answers structural questions
("what calls X", "where is X defined", "what owns X") faster and more
correctly via Loomweave than by grep-and-read — and trusts the graph because
identity is stable (SEI) and the extraction envelope is honest about its
limits.

## Authority grant

**Status: CONFIRMED** (proposed at bootstrap 2026-06-11; confirmed as drafted
by the owner the same day).

The agent acts autonomously **within** strategy:

- prioritize and reprioritize the backlog
- write PRDs for chosen bets
- dispatch delivery work (including to planning/implementation flows)
- accept work against stated acceptance criteria
- kill a failing bet (recording a PDR)

The agent **escalates before**:

- changing the vision, strategy, or this grant
- a public release, tag, or announcement (e.g. cutting 1.1.0 from rc4)
- deprecating a surface users or federation siblings depend on
  (MCP tools, HTTP API paths, SEI semantics, plugin protocol)
- any pricing/commercial change
- data deletion
- anything touching an external party (incl. pushing handoffs to the
  Wardline/Weft-hub repos — note the existing standing authorization covers
  *tool use*, not outward-facing publication)

Last reviewed: 2026-06-26 (confirmed unchanged by owner at `/own-product`
resume; prior 2026-06-24; first reviewed 2026-06-11). Review cadence: every 30
days or at each `/own-product` resume, whichever comes first.
