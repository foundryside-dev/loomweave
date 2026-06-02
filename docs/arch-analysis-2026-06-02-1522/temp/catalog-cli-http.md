## CLI surfaces + federation HTTP read API (clarion-cli, excluding analyze pipeline)

**Location:** `crates/clarion-cli/src/` — `main.rs`, `cli.rs`, `serve.rs`, `http_read.rs`, `install.rs`, `skill_pack.rs`, `hook.rs`, `hooks_settings.rs`, `mcp_registration.rs`, `doctor.rs`, `instance.rs`, `config.rs`, `db.rs`, `secret_scan.rs` + `secret_scan/{anchors,baseline,files,findings}.rs`, `stats.rs`

**Responsibility:** Owns the `clarion` binary's operator-facing entry surface: five subcommands (`install`, `analyze`, `serve`, `hook`, `db`, `doctor`), the `.clarion/` filesystem layout initialiser, the federation HTTP read API (file registry, call-graph linkages, SEI identity resolution, Wardline taint-fact store), the `serve` supervisor that runs MCP stdio and the Axum HTTP server as two runtimes sharing one `ReaderPool`, plus orientation-asset management (skill pack, SessionStart hook, `.mcp.json` MCP registration, `doctor` health check). The secret-scan entry point (`secret_scan.rs` pre-ingest gate) lives here; the detection library is `clarion-scanner`.

---

### Key components

- **`main.rs`** (119 LOC) — binary entry. Six-arm `match` on `cli::Command`. `dotenvy::dotenv()` is loaded for `install`/`serve`/`hook`/`db`/`doctor` but **deliberately skipped for `analyze`** (`main.rs:31-33`) so `.env` contents flow through the secret scanner rather than into plugin subprocess envs. New flags visible vs prior catalog: `run_id`, `resume`, `prune_unseen`, `progress_file`, `no_sei`, `no_incremental`, `legis_url` passed to `analyze`; `Hook` and `Db` subcommand arms added; `Doctor` arm added.

- **`cli.rs`** (192 LOC) — clap derive definitions. Six subcommands. `Analyze` grew to 9 flags (added `run_id`, `resume`, `prune_unseen`, `progress_file`, `no_sei`, `no_incremental`, `legis_url`). `Hook::SessionStart`, `Db::Backup`, and `Doctor` are new since prior catalog.

- **`serve.rs`** (364 LOC) — `serve::run` + `supervise_stdio_with_http`. Spawns HTTP server thread (multi-thread tokio runtime), MCP stdio thread (current-thread tokio runtime) sharing one 16-conn `ReaderPool`. Supervisor polls stdio `result_rx` with 100 ms timeout while `check_running`-polling HTTP. `Arc::ptr_eq` identity proof at `serve.rs:80-87` guards against a refactor that re-opens the pool inside `http_read::spawn`. `build_llm_provider` (`:269-329`) wires OpenRouter, Codex CLI, Claude CLI providers. Filigree ephemeral-port discovery via `clarion_mcp::filigree_url::resolve_filigree_url` at `serve.rs:48-58`.

- **`http_read.rs`** (4387 LOC) — the entire federation HTTP read API. `spawn` / `spawn_with_env` (`http_read.rs:182-305`), `run_http_read_server` (`:307-400`), `router` (`:452-527`). As of this analysis the router exposes **sixteen production routes** across three groups:
  - *`/api/v1/*` protected* (13): `GET /api/v1/files`, `POST /api/v1/files:resolve`, `POST /api/v1/files/batch`, `GET /api/v1/entities/:entity_id/callers`, `GET /api/v1/entities/:entity_id/callees`, `POST /api/v1/entities/callers:batch-get`, `POST /api/v1/entities/callees:batch-get`, `POST /api/v1/identity/resolve`, `POST /api/v1/identity/resolve:batch`, `GET /api/v1/identity/sei/:sei`, `GET /api/v1/identity/lineage/:sei`.
  - *`/api/v1/*` unprotected* (1): `GET /api/v1/_capabilities`.
  - *`/api/wardline/*` protected* (3, disabled-by-default write): `POST /api/wardline/resolve`, `POST /api/wardline/taint-facts` (write, off unless `wardline_taint_write: true`), `GET+POST /api/wardline/taint-facts` + `POST /api/wardline/taint-facts:batch-get` (read).
  Auth middleware (`:532-631`): `require_http_identity_with_limit` — HMAC preferred when `identity_secret.is_some()` (`:558-559`), falls back to bearer when `auth_token.is_some()` (`:561-579`), else trust-loopback with operator WARN.
  Tower stack (`:514-527`): `CatchPanicLayer` → `HandleErrorLayer` → `TraceLayer` → 10 s `TimeoutLayer` → `LoadShedLayer` → `ConcurrencyLimitLayer(64)`. Body limits: 16 KiB (`HTTP_BODY_LIMIT_BYTES`, `:776`) on v1 group; 4 MiB (`WARDLINE_BODY_LIMIT_BYTES`, `:789`) + axum `DefaultBodyLimit::max` (`:510-513`) on wardline group.
  Optional ADR-036 taint writer-actor spawned **inside** the HTTP runtime at `:362-372` — a bounded second writer on one DB (comment at `:345-352` cites ADR-036 §4 relaxation; serialises at SQLite write lock rather than corrupting).

- **`install.rs`** (389 LOC) — `InstallPlan` enum (`:114-157`) replaces prior three-bool form; bare `clarion install` → `All` (init + skills + hooks); `--skills`/`--hooks` → `Components`. `populate_after_mkdir` (`:278-301`) creates `.clarion/{clarion.db, config.json, .gitignore}` + `clarion.yaml` stub. Cleanup guard (`:225-238`) `rm -rf`s `.clarion/` if any post-mkdir step fails. Now drives `skill_pack::install_skill_pack` and `hooks_settings::install_session_start_hook` as named components.

- **`skill_pack.rs`** (436 LOC) — bundles the `clarion-workflow` SKILL.md via `include_str!` from `clarion-mcp/assets/skills/clarion-workflow/SKILL.md` (`:19-22`). Drift-aware re-copy keyed on blake3 fingerprint over `(rel_path, contents)` pairs (`:35-44`). Installs into `.claude/skills/clarion-workflow/` and `.agents/skills/clarion-workflow/` (`:49`). Atomic write via stage-and-rename (parallel to `hooks_settings`).

- **`hooks_settings.rs`** (762 LOC) — `.claude/settings.json` SessionStart-hook merge. `HookState` enum: `Present` / `Stale` / `Missing` / `Unparseable`. `desired_hook_command` (`:43`) generates `clarion hook session-start --path <single-quoted path>` with POSIX single-quote escaping (`:102-114`) to defend against shell-metacharacter injection in project paths containing `$`, backticks, or backslashes. `merge_session_start_hook` (`:128-233`) canonicalises to exactly one Clarion-owned hook: refreshes the first, removes extras. Never-clobber on disk write (`:265-290` refuses to rewrite a wrong-type `hooks` or non-array `SessionStart`). Atomic write via temp-and-rename (`:312-316`).

- **`mcp_registration.rs`** (344 LOC) — `.mcp.json` Clarion server-entry detection and never-clobber merge. `McpState` enum. `install_mcp_entry` (`:107-188`) merges `mcpServers.clarion` preserving existing `command`/`type`/`env`, only correcting `args`. Atomic write via PID-suffixed temp file (`:182-186`).

- **`doctor.rs`** (177 LOC) — `clarion doctor [--fix]`. Three-surface health check: skill pack, SessionStart hook, `.mcp.json` entry. Per-surface ✓/✗ output. With `--fix` calls each module's idempotent installer; convergence is verified post-repair. Exits non-zero if any surface unhealthy (CI-usable gate). Also prints index snapshot via `hook::snapshot_report`.

- **`hook.rs`** (193 LOC) — `clarion hook session-start` entrypoint. Prints project snapshot; re-syncs skill pack on drift. Always exits 0 (fail-soft, so a misbehaving hook never blocks session start). Provides `snapshot_report` helper reused by `doctor`.

- **`secret_scan.rs`** (574 LOC) + `secret_scan/{anchors,baseline,files,findings}.rs` — pre-ingest secret scan driver. `SecretScanOptions::from_cli` (`:46-62`), `pre_ingest` entry point. `collect_scan_files` (via `files.rs`) does a broader walk than `analyze`'s source walk (includes `.env` sidecars). Scanner runs in parallel via `thread::scope` with `available_parallelism()` workers. Classifies each file as `Clean`/`Blocked`/`Overridden`, producing `BTreeSet<PathBuf>` shared (immutably) into every `PluginHost`. Submodules: `anchors.rs` (links detections to plugin-emitted entities), `baseline.rs` (loads `.clarion/secrets-baseline.yaml`), `files.rs` (extension/skip-dir walk + sidecar matcher), `findings.rs` (`PendingFinding` shaped with rule IDs `CLA-SEC-SECRET-DETECTED` / `CLA-SEC-UNREDACTED-SECRETS-ALLOWED`).

- **`instance.rs`** (148 LOC) — `InstanceId(Uuid)` newtype. Persisted to `.clarion/instance_id` with `O_CREAT|O_EXCL` (mode 0600) → fsync → hard_link for atomic publish. Handles EEXIST-on-link → read-existing-file race. Federation contract requires a stable per-project ID.

- **`config.rs`** (141 LOC) — `AnalyzeConfig` only (clustering knobs). `HttpReadConfig` and `McpConfig` trust-matrix validation live in `clarion-mcp::config`. The CLI-side `config.rs` does NOT contain the ADR-034 refuse-to-start trust matrix; that is delegated entirely to `clarion_mcp::config::HttpReadConfig::validate_auth_trust` and `validate_loopback_trust`, called in `http_read::spawn_with_env` at `http_read.rs:217-222`.

- **`db.rs`** (135 LOC) — `clarion db backup` — WAL-safe online SQLite backup via `rusqlite::Connection::backup`. Accepts `--force` to overwrite.

- **`stats.rs`** (37 LOC) — thin `PluginStats` struct serialisation for per-run stats reporting.

---

### Dependencies

- **Inbound:** No Rust callers. Verified by: `grep -rn "clarion_cli" crates/` returns empty. Callers are human operators, CI scripts, and `tests/e2e/*.sh`.
- **Outbound (other Clarion crates):**
  - `clarion-core`: `AcceptedEntity`, `AcceptedEdge`, `discover`, `PluginHost`, `CrashLoopBreaker`, `BriefingBlockReason`, `HostError`, `HostFinding`, `HttpErrorCode`, LLM provider types, `EdgeConfidence`, `EntityVisibility`.
  - `clarion-mcp`: `ServerState`, `config::{McpConfig, HttpReadConfig, LlmConfig, ProviderSelection}`, `filigree::{FiligreeHttpClient}`, `filigree_url::resolve_filigree_url`, `serve_stdio_with_state_on_runtime`, `select_provider_with_env`, `DiagnosticsContext`, `LlmDiagnostics`. Critically, `HttpReadConfig::validate_auth_trust` and `validate_loopback_trust` live in `clarion-mcp`, not here.
  - `clarion-storage`: `Writer`, `WriterCmd`, `ReaderPool`, `ReaderPool::open_validated`, `resolve_file_catalog_entry`, `resolve_locator`, `resolve_sei`, `sei_lineage`, `call_edges_from`, `call_edges_targeting`, `entity_visibility`, `is_reserved_sei`, `CanonicalProjectPath`, `StorageError`, `SeiLookupResult`, `CallEdgeMatch`, `EntityVisibility`, `pragma`, `schema`.
  - `clarion-scanner`: `Scanner`, `Detection`, `Baseline`, `SuppressionResult`.
- **External crates of note:** `axum` 0.7 (HTTP routing), `tower`/`tower-http` (middleware stack), `tokio` (two runtimes: multi-thread for analyze + HTTP; current-thread for MCP stdio), `clap` (CLI), `blake3` (skill-pack fingerprint), `sha2` (HMAC + cluster hash), `xgraph` (Leiden clustering in analyze pipeline), `ignore::WalkBuilder` (gitignore-honouring tree walk), `rusqlite` (direct connections in install, analyze phase 3, run_lifecycle, db backup), `uuid`, `dotenvy`, `serde_json`, `serde_norway` (YAML config), `tempfile` (tests).
- **External services / processes:** SQLite at `<project>/.clarion/clarion.db`; TCP listener for HTTP read API on `config.serve.http.bind` (default `127.0.0.1:9111`); plugin subprocesses via `clarion_core::PluginHost`; LLM CLIs (codex/claude) and OpenRouter HTTPS.

---

### Patterns observed

- **Shared `Arc<()>` identity tag across runtimes** (`serve.rs:80-87`, `http_read.rs:52-63`, `:320-325`). The HTTP thread captures `readers.identity()` *after* the pool is moved into the runtime; supervisor checks `Arc::ptr_eq` against its own handle. A refactor that re-opens the pool inside the HTTP thread ships the new identity back; the assert fires.
- **Auth precedence chain: HMAC > bearer > loopback-trust** (`http_read.rs:558-579`). `require_http_identity_with_limit` checks `identity_secret.is_some()` first (HMAC path), then `auth_token.is_some()` (bearer), else passthrough with a WARN at bind time (`:235-296`). Two WARN variants: loopback-without-auth (`:243-244`) and non-loopback-without-auth (`:235-238`). HMAC canonical message: `"{METHOD}\n{path_and_query}\n{hex(sha256(body))}"` (`:640-647`). Hand-rolled HMAC-SHA256 (`:649-671`), constant-time compare on both paths (`:576, 618`).
- **Two-pass body read for HMAC** (`http_read.rs:588-621`). `to_bytes(body, limit)` to compute digest, then `Request::from_parts(parts, Body::from(body_bytes))` reconstructs the request for downstream handlers. Two separate limit constants used: `HTTP_BODY_LIMIT_BYTES` (16 KiB) for v1 routes, `WARDLINE_BODY_LIMIT_BYTES` (4 MiB) for wardline routes (`:537`, `:549`).
- **Never-clobber JSON merge** in `hooks_settings.rs` (`:265-290`) and `mcp_registration.rs` (`:123-138`). Both refuse to rewrite if the existing file has a wrong-type `hooks`/`mcpServers` key; both use temp-and-rename atomic write.
- **POSIX single-quote escaping for shell injection defence** (`hooks_settings.rs:102-114`). Hook command embeds project path via `shell_single_quote`, preventing `$`, backtick, and `\` expansion. Verified by `shell_quote_round_trips_metacharacters_through_a_real_shell` unit test that runs actual `sh`.
- **Optional ADR-036 taint writer-actor inside HTTP runtime** (`http_read.rs:362-372`). Spawned only when `wardline_taint_write: true`. Sender-lifetime discipline (`:353-362` comment): `Writer` handle dropped at block end, only `writer.sender()` in `AppState`; `taint_writer_join` held outside `AppState` so it survives `serve_future` consumption and is awaited at shutdown. Two writer-actors on one DB is a bounded ADR-011 relaxation; serialises at SQLite busy_timeout.
- **Project-guard semantics** (`http_read.rs:149-167`). `AppState::reject_project_mismatch` treats an empty `project` field as "no assertion" (Wardline may omit it), rejects a non-empty mismatch with `403 PROJECT_MISMATCH`. Guard, not selector — one `serve` serves exactly one project.
- **`InstallPlan` enum encoding** (`install.rs:114-157`). The prior three-bool form allowed illegal do-nothing states; replaced with a typed enum whose `from_flags` constructor cannot produce `{false, false, false}`. Verified by `from_flags_never_yields_a_do_nothing_plan` test.
- **Fail-loud unenumerated middleware error** (`http_read.rs:694-719`). `handle_middleware_error` panics with the full error chain for any `BoxError` that is not `Elapsed` or `Overloaded`; `CatchPanicLayer` (`:516`) catches the panic and returns a 500 `INTERNAL` envelope, while CI sees the missing enumeration as a hard failure rather than a silent swallow.
- **`cfg(test)` cooperative panic hook** (`http_read.rs:419-441`). `HTTP_THREAD_PANIC_TRIGGER` static atomic lets tests assert the supervisor's runtime-internal-panic arm still fires after `CatchPanicLayer` was introduced (which absorbs per-request handler panics).

---

### DRIFT — code vs §1/§9, contracts.md, ADR-014/034, CLAUDE.md

**1. Route count divergence: system-design.md §9 is frozen at 1.0 scope, contracts.md is the live wire contract.**

- `system-design.md:1007` documents `GET /api/v1/entities/resolve?scheme=wardline_qualname` as a shipped public API. This route does **not exist** in `http_read.rs` (router at `:452-527` has no `/api/v1/entities/resolve` path). `contracts.md:776-781` confirms this is deferred (`POST /api/wardline/resolve` ships instead; the normalising endpoint is "named here, not silently dropped"). Precedence: contracts.md + newer ADRs pin the live surface; system-design §9 is the frozen 1.0 docset. **Not a code defect — a version-boundary doc-lag in system-design.md.**

- The router now exposes **sixteen production routes** vs the four documented in the prior (1.0) catalog. The new routes (call-graph linkages WS2, SEI identity resolution WS1/ADR-038, Wardline taint-fact store SP9/ADR-036) all have pinned contracts in `contracts.md` and are absent from `system-design.md §9`. Again version-boundary lag, not code drift.

- `system-design.md §9` does not reference ADR-036, ADR-037, or ADR-038. `contracts.md` covers all three. Going forward, any reader relying on system-design.md §9 for the live HTTP surface will get an incomplete picture; the doc hierarchy makes contracts.md authoritative.

**2. `UNAUTHORIZED` vs `UNAUTHENTICATED` (prior handover flag) — RESOLVED, no live defect.**

- Prior handover flagged this as a concern. Current code: `StatusCode::UNAUTHORIZED` (the HTTP/1.1 401 reason phrase, `http_read.rs:627`) with wire `code: "UNAUTHENTICATED"` (from `ErrorCode::Unauthenticated::as_str()` → `"UNAUTHENTICATED"`, `clarion-core/src/errors.rs:73`). `contracts.md:74-79` pins the 401 wire response as `{"error": "authentication required", "code": "UNAUTHENTICATED"}`. Code and contract agree. **Not a live defect.**

**3. ADR-034 auth trust matrix — code matches contracts.md; trust-matrix validation delegated to clarion-mcp.**

- Auth precedence chain HMAC > bearer > loopback-trust matches contracts.md §Authentication trust matrix exactly. Constant-time comparison on both paths confirmed (`http_read.rs:576`, `:618`). `WARN` log on loopback-without-auth (`http_read.rs:289-297`) and non-loopback-without-auth (`:282-288`) match the `[TRUST]` documentation.
- **The refuse-to-start matrix** (`CLA-CONFIG-HTTP-IDENTITY-MISSING` / `CLA-CONFIG-HTTP-NO-AUTH` per contracts.md:66-69) is enforced by `HttpReadConfig::validate_auth_trust` and `validate_loopback_trust` in `clarion-mcp`, called at `http_read.rs:217-222`. Not visible in this crate; see clarion-mcp analysis for full trust-matrix verification.

**4. `_capabilities` response matches contracts.md §`GET /api/v1/_capabilities`.**

- Handler at `http_read.rs:2155-2167` returns `{registry_backend: true, file_registry: true, api_version: 1, instance_id, linkages: {http: true}, sei: {supported: true, version: 1}}`. Contracts.md:445-456 pins exactly this shape. Conformant.
- contracts.md:837-841 states `_capabilities` does NOT advertise the taint store or whether write is enabled. Code agrees: `get_capabilities` does not expose `wardline_taint_write` state.

**5. Wardline taint-fact store body-limit composition.**

- Code applies `RequestBodyLimitLayer::new(WARDLINE_BODY_LIMIT_BYTES)` AND `DefaultBodyLimit::max(WARDLINE_BODY_LIMIT_BYTES)` to the wardline group (`http_read.rs:510-513`). This matches contracts.md §wardline framing (4 MiB). The comment at `:481-495` explains the composition rationale (axum's `Json` extractor has a 2 MB default that tower-http's layer does not touch). Conformant.

**6. HMAC over-limit body read — body-read failure now returns 500 INTERNAL.**

- Prior catalog noted the body-read failure branch returned an internal error response. Comment at `http_read.rs:605-615` confirms this was an explicit fix (`CI-02 fix`) to return 500/`INTERNAL` rather than mis-classifying a transport failure as a path defect. Conformant with contracts.md's closed error envelope.

---

### Quality concerns / debt

- **[HIGH] `http_read.rs` is 4387 LOC in a single file.** All routes, all auth, both error envelopes, both body-limit groups, SEI handlers, Wardline handlers, and the test-only panic harness coexist. The protected/unprotected sub-router split is already in place (`:452-527`); each handler family (files, linkages, identity/SEI, wardline, capabilities) is a natural module boundary. The growth path is clear: `SP9` Wardline routes alone (3 handlers + request/response structs + freshness logic) account for several hundred LOC. Size is a concrete change-risk metric — any cross-cutting modification (new auth variant, new error code, body-limit reconfiguration) requires reading across the full file. A refactor into `http_read/{files,linkages,identity,wardline}.rs` submodules would not change the public surface. No immediate correctness risk, but high maintenance friction at this scale.

- **[MEDIUM] Two writer-actors on one DB when `wardline_taint_write: true`** (`http_read.rs:362-372`). The comment at `:345-352` correctly identifies this as a bounded ADR-011 relaxation authorised by ADR-036 §4; both actors open `BEGIN IMMEDIATE` under `busy_timeout + capped-backoff`, so they serialise at the SQLite write lock. The risk is not correctness under normal operation but operational complexity: an operator enabling the taint-store write API implicitly starts a second writer-actor in the HTTP runtime with no separate health-check surface (it is awaited at shutdown, not polled). If the writer-actor silently stalls, the HTTP runtime's serve loop continues but writes queue indefinitely. A heartbeat or channel-lag metric on the taint writer would improve observability.

- **[MEDIUM] System-design.md §9 is stale relative to the live wire surface.** Sixteen production routes exist; §9 documents four. Any reader who uses system-design.md as their HTTP surface reference will miss call-graph linkages, SEI identity resolution, Wardline routes, and project-guard semantics. The fix is a §9 update to reference contracts.md as the authoritative live-surface document, or a diff-note cross-link. contracts.md is the correct source of truth, but navigators who hit system-design.md first get an incomplete picture.

- **[LOW] HMAC is hand-rolled** (`http_read.rs:649-671`). The implementation is correct (standard HMAC-SHA256 structure, constant-time compare paired), but it bypasses the `hmac` + `sha2` crate pair's `Hmac<Sha256>` type. The maintenance bet is that any future HMAC variant (key rotation, HKDF derivation) reopens this file rather than gaining a crate-level upgrade path. The body-hash step (Sha256 of body before HMAC) is not a standard HMAC pattern; it adds a custom canonical message format that needs matching on the caller side. Any change to the canonical message format is a silent breaking change to federation clients. Documented in contracts.md; risk is manageable but worth tracking.

- **[LOW] Per-group body-limit composition is subtle.** The `v1` 16 KiB limit is applied to the *already-merged* v1 router *before* merging with wardline (`:487-489`). The wardline 4 MiB limit + `DefaultBodyLimit::max` is applied to the wardline sub-router only (`:510-513`). The comment at `:480-495` explains why the flat-merge approach would cap wardline at 16 KiB. This is correct but non-obvious: a future developer adding a new route group who applies the body limit in the wrong composition order will silently cap (or not cap) body sizes.

- **[LOW] `handle_middleware_error` panic for unenumerated `BoxError`** (`http_read.rs:694-719`). The design intent is sound (force enumeration via CI failure rather than silent swallow). In production the user sees 500/`INTERNAL`; the panic message goes to stderr only via `CatchPanicLayer`. A runbook note or an additional `tracing::error!` before the panic would make the failure discoverable without grepping stderr.

- **[INFO] `config.rs` is analyze-only** (141 LOC, `AnalyzeConfig` + `ClusteringConfig`). `HttpReadConfig` and the trust-matrix validators are entirely in `clarion-mcp`. This split is correct by design (serve config is MCP-layer concern) but worth stating explicitly so a reader of this crate does not hunt for HTTP config validation here.

---

**Confidence:** High — Read 100% of all non-analyze-pipeline source files end-to-end: `main.rs`, `cli.rs`, `serve.rs`, `install.rs`, `skill_pack.rs` (first 80 LOC + structure), `hooks_settings.rs` (all 762 LOC), `mcp_registration.rs` (all 344 LOC), `doctor.rs` (all 177 LOC), `hook.rs` (structure), `instance.rs` (structure), `config.rs` (all 141 LOC), `secret_scan.rs` (first 80 LOC + submodule list). Read `http_read.rs` sections: spawn/bootstrap (`:1-305`), runtime bootstrap + auth + HMAC (`:305-692`), router (`:452-527`), all handler stubs + capabilities (`:945-2167`), error classifier + middleware helpers (`:2183-2316`), and test stubs (`:2318-2500`). Cross-validated route list against `contracts.md` (full read). Read `docs/clarion/1.0/system-design.md` §9 excerpts (grep-located). Verified `ErrorCode::Unauthenticated.as_str()` in `clarion-core/src/errors.rs`. Did NOT open the fixture JSONs (shape-level per-endpoint conformance was not verified against normative fixtures), the test files end-to-end (5000+ LOC), or `clarion-mcp::config::HttpReadConfig` (trust-matrix implementation is cited by call site, not verified by reading the callee). `hooks_settings.rs` is marked NEW in the task; full read completed.
