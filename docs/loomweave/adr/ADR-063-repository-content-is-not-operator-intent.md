# ADR-063: Repository Content Is Not Operator Intent

**Status**: Accepted
**Date**: 2026-09-02
**Deciders**: john@foundryside.dev
**Context**: Loomweave holds an untrusted-corpus posture (ADR-045) but read three inputs from *inside* the analysed tree as if the operator had written them. `<project_root>/loomweave.yaml` names network endpoints (`llm_policy`, `semantic_search`, `integrations.filigree`, `serve.http`) and the **name** of the env var whose value is sent as a bearer token — so a committed config could exfiltrate any operator env var plus source-derived text on the first `analyze`, including the hook-spawned background one, with `allow_live_provider` living in the same attacker-controlled file. `<project_root>/.venv/bin/python` (ADR-058 rung 2) is *executed* by pyright, so a committed binary there is code execution as the operator. And every git probe against the corpus ran with no deadline and unbounded output, so a hostile or pathological repository could wedge `serve`, the hook, or `analyze` indefinitely. Tickets: clarion-dee44f1a66 (P1), clarion-9b3cf287b7 (P2), clarion-9202f4acec (P2).
**Glossary verdict**: **no clash** — `config_trust`, `ConfigTrust`, `tracked_state` / `TrackedState`, and the state vocabulary (`operator_owned`, `not_a_git_work_tree`, `git_unavailable`, `repository_tracked`, `unknown`) are Loomweave-local; no sibling product uses these terms, and the shared federation term `tracked` is not redefined here. No `docs/suite/glossary.md` change required — that in-repo file is a pointer stub whose only table records *managed* clashes (the canonical catalogue is `~/weft/glossary.md`), so a no-clash verdict is recorded here, per the ADR-040 precedent.

## Summary

**Content tracked by the analysed repository is repository content, never operator intent.** Operator intent is expressed only by files the operator owns — untracked files in the checkout, the real environment, and CLI flags.

The primitive is one question — "is this path tracked by git?" — answered through a hardened, bounded git probe (`loomweave_core::tracked_state`, with a conformance-tested Python twin). Two consumers apply it and both fail **closed**: a repository-tracked `loomweave.yaml` loses its egress-capable sections before anything reads them, and a repository-tracked `.venv/bin/python` is skipped on ADR-058's rung 2. One sentence for the config half: *a tracked config may shape analysis; it may not name a network endpoint, a credential, or a listen interface.*

## Context

- `loomweave.yaml` is written at the project root by `loomweave install`, i.e. inside the very tree Loomweave analyses. Nothing distinguished "the operator put this here" from "the repository shipped it", and `analyze` runs with no credentials by design — which made the file the only thing choosing whether credentials were reached for at all.
- The threat is not a malicious operator; it is an ordinary operator opening an unfamiliar checkout, whose agent harness launches `serve` and whose session hook launches `analyze` before a human has read anything in the tree.
- `.env` was closed one day earlier by ADR-062 (parsed into a private sidecar, never the process environment). The same review found the config and interpreter cases, which ADR-062 explicitly deferred here.
- The bounded-probe half is prerequisite, not incidental: the trust question is answered by *spawning git against corpus content*. An unbounded probe would let a hostile repository turn every trust check into a hang.

## Decision

1. **The principle.** Repository-tracked content is repository content. Every trust decision that reads from inside the analysed tree asks whether git tracks the path, and treats a positive answer as untrusted input rather than configuration.

2. **One primitive, fail-closed by default.** `loomweave_core::tracked_state(root, path) -> TrackedState` answers with five states: `Tracked`, `Untracked`, `NotAGitWorkTree`, `GitUnavailable`, `Unknown(GitProbeError)`. It runs one `git --literal-pathspecs ls-files -z` over the path, each ancestor up to the root, and — when the path canonicalises inside the root — the canonical path and *its* ancestors, so a committed file, a committed symlink at any level, a committed directory, and a symlink into committed content all read as `Tracked`. `treat_as_tracked()` is `Tracked | Unknown`: a probe that cannot answer fails closed, because a checkout git itself refuses to read is an untrusted-corpus signal. `GitUnavailable` is the one deliberate exception — `PATH` is real-environment-only (ADR-062), so a missing git binary is the **operator's** environment, not repository content, and treating it as tracked would let an operator's own toolchain gap silently disable their provider. The Python plugin carries a twin (`git_trust.py`) returning the same five labels; the shared vectors are `fixtures/git_tracked_paths.json`, consumed by both suites. Change both or neither.

3. **The config gate.** `McpConfig::load_trusted(path)` — the only loader every full-config consumer uses (`serve`, `analyze`'s `load_mcp_config`, `config check`, `doctor`, and the MCP config-read surfaces) — parses, decides ownership, resets the egress-capable sections, and *then* validates. The strip precedes validation deliberately: a hostile-but-invalid egress section would otherwise turn the gate into a startup failure, letting the corpus choose whether Loomweave runs at all.

   | section | tracked ⇒ |
   |---|---|
   | `llm_policy` | `LlmConfig::default()` (disabled) |
   | `semantic_search` | `SemanticSearchConfig::default()` (disabled) |
   | `integrations` | `IntegrationsConfig::default()` (no Filigree/Warpline endpoint or `token_env`) |
   | `serve.http` | `HttpReadConfig::default()` (loopback, no `identity_token_env`) |
   | `version`, `analysis`, `serve.mcp` | honoured |

   The verdict is `ConfigTrust` (`operator_owned` / `not_a_git_work_tree` / `git_unavailable` / `repository_tracked` / `unknown`), carries the list of sections it actually reset, and is announced once per process — `warn` when sections were stripped, `info` otherwise. The trust probe is rooted at the **config file's own directory** with a bare-filename pathspec: rooting it at the true repository root would probe the file's ancestor directories too, and `git ls-files -- <dir>` is non-empty whenever any sibling under it is tracked, which would misclassify almost every operator-owned config as repository content.

4. **Writers refuse a tracked target.** `update_llm_config_file` / `update_semantic_config_file` — and therefore `llm_config_set` / `semantic_config_set` (MCP) and `config llm set` / `config semantic set` (CLI) — refuse to edit a repository-tracked config, with the verbatim remedy `CONFIG_TRACKED_REMEDY` in the message and the stable code `LMWV-CONFIG-REPOSITORY-TRACKED`. On the MCP surface the refusal is answered in the **invalid-params class** (JSON-RPC `-32602`), not as a storage error: it is a caller-side fault, not something to retry. Without this, a bootstrap from a read-only agent session would happily write a config the operator could never use.

5. **Rung 2 of interpreter discovery is trust-conditioned** (amendment to ADR-058). `<project_root>/.venv/bin/python` is accepted only when its tracked state is `Untracked`, `NotAGitWorkTree`, or `GitUnavailable`; on `Tracked` or `Unknown` the rung is skipped exactly as if the file were absent and resolution continues at `VIRTUAL_ENV` / `CONDA_PREFIX` / `PATH`. Both sides implement the predicate and both log once. No new `InterpreterSource` variant: the outcome is whatever the next rung yields, and `interpreter_unpinned` (ADR-057) already explains the degradation. `pyrightconfig.json`'s `venvPath` / `venv` are **not** gated — verified against the pinned pyright bundle, those keys only shape search-path globbing and never execute a program.

6. **Every corpus git probe is bounded.** `run_git_probe(command, limits)` (default 30 s deadline, 32 MiB stdout cap) drains stdout and stderr on dedicated threads started before `wait`, kills the process *tree* on deadline or overflow, always joins its readers and always reaps the child. stderr is a 64 KiB ring and is diagnostic only. All fifteen corpus-facing call sites route through it, and strict UTF-8 replaces lossy decoding — each site failing in the direction it already documents as fail-soft. No fail-soft site turns a probe error into a *positive* verdict: `doctor`'s db-tracked check gained an `Unknown` state rather than folding a timeout into "untracked".

## Consequences

### Positive

- The class closes for both inputs at once. A committed `loomweave.yaml` can no longer name an endpoint, a credential env var, or a listen interface, no matter which command reads it — `analyze`, the hook-spawned background `analyze`, `serve`, `doctor`, or `config`. A committed `.venv/bin/python` is never executed. Pinned by an end-to-end acceptance test: a fixture repository with a committed config pointing three egress sections at a local listener, plus `LOOMWEAVE_TEST_CANARY=leak` in the child environment, captures **zero** requests under `analyze` and under `serve`; the byte-identical config, untracked, populates embeddings through the same listener.
- The verdict is visible everywhere the symptom is: `project_status_get` and `llm_config_get` / `semantic_config_get` carry `config_trust`; `loomweave config check` and `config semantic status` print it first; `loomweave doctor` gains a gate-failing `config.trust` check with a `--fix`; `loomweave install` prints the ownership advisory. An operator whose provider "silently stopped working" is told why in whichever surface they reach for.
- Trust decisions can no longer be wedged by the corpus: every probe behind them has a deadline and an output cap, and kills the whole process tree rather than the direct child.
- The bounded runner is a general asset — it also removed the leaked-reader-thread and unreaped-child patterns from the prior art it replaces.

### Negative

- **A team that committed `loomweave.yaml` loses its egress settings until they untrack it.** This is the intended behaviour and the intended cost; it is a real behaviour change for anyone who was using a committed config as a team default. It is surfaced four ways rather than silently: the once-per-process `warn` on every `serve` / `analyze`, `doctor`'s `config.trust` problem (with `--fix`), the `config check` trust line, and the `install` advisory. The `analysis` block is still honoured, so shared clustering settings keep working — only egress is inert.
- `git_unavailable` is permissive by construction. An operator with no git binary keeps their config's egress sections, and Loomweave says so (`doctor` warns) rather than pretending it verified anything.
- A checkout owned by another uid makes git refuse with "dubious ownership", which reads as `Unknown` and therefore fails closed. That is a deliberate false-positive: `safe.directory` is operator state we do not consult (see Residuals).
- Trust decisions now spawn git where they previously spawned nothing. Each is one bounded `ls-files`, on config load and on interpreter resolution — not per file — but it is not free, and it is a new failure surface on an already-hostile input.

## Alternatives considered

- **An operator-level config outside the tree (XDG).** Deferred, not rejected on merit. No such location exists today, every operator already has an untracked `loomweave.yaml` path available, and adding a second precedence ladder is a larger change than the threat needs. If a "team default config" workflow turns out to be genuinely load-bearing, this is where it should land — as a *new* rung, never by re-trusting the tracked file.
- **Allowlisting credential env-var names or endpoint hosts.** Rejected. An operator's untracked config is trusted, so an allowlist buys nothing there; against a tracked config it is strictly weaker than making the whole section inert, and it would break legitimate local providers (Ollama on a LAN host, a self-hosted OpenAI-compatible endpoint) for no security gain.
- **Closing Codex #142 and #147 as written.** Rejected. #142 dropped the `.venv` interpreter rung altogether, which reverses ADR-058 and re-opens the launcher-dependent resolution bug — the whole reason rung 2 exists. #147 added a CLI consent flag for analyze-time embeddings, which disables operator-enabled embeddings on every real launch path (the hook-spawned `analyze` has no human to consent) and covers one of two egress surfaces. Both were re-scoped to the narrow, trust-conditioned form above.
- **Command-level env scrubbing instead of a trust verdict.** Considered and rejected for the config case: the problem is not what a child inherits (ADR-062 closed that) but what the *parent* is told to do. Only refusing to honour the instruction fixes it.

## Residuals

Known and deliberately not closed here. Each is a recorded exposure, not a to-do implied by this ADR.

- **`weft.toml`'s `[loomweave]` keys** are repository-adjacent and ungated. They can relocate the store; they do not name endpoints or credentials, which is why they sit below this gate — but they are corpus-adjacent input read as operator intent.
- **A committed `.env`** remains a credential *source* under ADR-062: it can supply a value for a variable Loomweave's own config names. ADR-062's guarantee is that it never enters the environment and no child inherits it; it is not a guarantee that its values are unused.
- **A grandchild holding the pipes after a tree kill** is bounded only by the probe deadline — the reader threads cannot finish until every writer end closes. On non-Linux targets the tree kill degrades to killing the direct child only.
- **`doctor --fix`'s `current_exe analyze` spawn is unbounded.** It is not a git probe and was explicitly out of scope; a wedged classifier repair still hangs `doctor --fix`.
- **A checkout owned by another uid** makes git refuse with "dubious ownership", which reads as `Unknown` and fails closed — the config's egress sections are stripped and rung 2 is skipped. The operator's `safe.directory` setting is deliberately **not** consulted: reading it would reintroduce a config-driven path into the trust decision.
- **Fingerprint alternation.** A `PATH`-narrowed `analyze` (no git on `PATH` → `GitUnavailable` → rung 2 accepted) and an operator `analyze` (git present → `Tracked` → rung 2 skipped) can resolve different interpreters on the same tree, and the ADR-058 `resolver_environment` fingerprint then forces a full plugin re-dispatch on each alternation.
- **`loomweave install` merges** Filigree and HTTP settings into an existing `loomweave.yaml` (`integration_bindings::install_loomweave_yaml`) without consulting trust. The merged settings are inert under this gate, but the merge still dirties a tracked file. Gating it was left out of scope; the ownership advisory names the situation instead.
- **The config trust probe is rooted at the file's own directory** (see Decision 3). A `loomweave.yaml` symlinked to committed content *elsewhere in another repository* is therefore not chased and reads as untracked. Widening the root is not the fix — it would misclassify almost every operator-owned config.
- **The `analyze` fast path is skipped permanently for a repository with a non-UTF-8 path in a commit range.** Strict UTF-8 decoding fails the probe, which fails soft into "take the full analyze" — correct, but a permanent cost for that repository rather than a one-off.

## Related Decisions

- **Builds on**: [ADR-045](./ADR-045-worktree-source-staleness.md) (the untrusted-corpus posture this generalises), [ADR-013](./ADR-013-pre-ingest-secret-scanner.md) (the corpus is scanned, not trusted), [ADR-062](./ADR-062-dotenv-credential-sidecar.md) (`.env` is a credential sidecar; `PATH` and launcher overrides are real-environment-only, which is why `GitUnavailable` is permissive here).
- **Amends**: [ADR-058](./ADR-058-project-interpreter-discovery.md) — rung 2 gains the tracked-state condition (`## Amendment (2026-09-02)`).
- **Related to**: [ADR-057](./ADR-057-pyright-restart-attribution.md) (`interpreter_unpinned` explains the rung-2 skip's downstream effect), [ADR-061](./ADR-061-plugin-process-tree-and-serve-parent-liveness.md) (the process-tree kill the bounded probe reuses).
- **Tickets**: clarion-dee44f1a66 (config egress gate), clarion-9b3cf287b7 (`.venv` rung trust), clarion-9202f4acec (bounded git I/O). Supersedes the Codex proposals #142 and #147, both closed in favour of the narrow forms above.
