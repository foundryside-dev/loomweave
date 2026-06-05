# Request to Filigree — `scan_source`-scoped finding retention/prune surface (+ scan-run-create contract decision)

> **⚠️ PRUNE ASK WITHDRAWN / SUPERSEDED (2026-05-30).** The prune surface this
> memo requested **already exists** in Filigree:
> `POST /api/weft/findings/clean-stale` with body
> `{"scan_source": "loomweave", "older_than_days": 30, "actor": "…"}` →
> `{"findings_fixed": N, "scan_source": "…", "older_than_days": N}`. It
> soft-archives `unseen_in_latest` findings to `fixed` (auto-reopen on
> reappearance; Filigree ADR-015), `scan_source` required server-side as an
> accident-guard. Verified against Filigree's handler
> (`src/filigree/dashboard_routes/files.py`) and its API tests. Loomweave's
> `--prune-unseen` now consumes it directly (REQ-FINDING-06 done); the contract
> is pinned in `docs/federation/contracts.md` → "Consumed Filigree route:
> clean-stale retention". My §1–§3 and §5 below (asking Filigree to *design and
> build* a prune surface) are therefore **moot** — the original "no prune route
> exists" premise was wrong. **§4 (scan-run-create contract decision) is the
> only live ask** and remains open.

**Status:** Prune ask withdrawn; §4 (scan-run-create) open (2026-05-30)
**Author side:** Loomweave
**Tracking issue (Loomweave):** §4 tracked under a `release:1.1` issue (Phase-0 scan-run-create handshake); the prune/`--prune-unseen` piece of `clarion-dd29e69e0e` is done.
**Sibling docs:**
- Loomweave `docs/federation/contracts.md` → "Consumed Filigree route: clean-stale retention" (the route Loomweave now consumes) and "…: scan-results intake".
- Loomweave `docs/loomweave/1.0/requirements.md` → REQ-FINDING-05, REQ-FINDING-06.
- Filigree intake handler for reference: `db.process_scan_results` (`db_files.py:857-926`, per ADR-014).

> **Note on placement.** This is a Loomweave-authored *request to* Filigree, kept here for Loomweave's reference. The authoritative Filigree-side artifact (design + implementation) should live in the Filigree repo; refresh `docs/federation/filigree-side/` with a mirror once Filigree drafts its response.

---

> The §1–§3 / §5 prune-design sections below are retained only as a record of
> the (mistaken) original request; see the withdrawal banner above. Skip to §4
> for the live question.

## 1. Problem in one paragraph

Loomweave emits findings into Filigree via `POST /api/v1/scan-results` with
`scan_source: "loomweave"`, `mark_unseen: true`, `complete_scan_run: true`. When
`mark_unseen` is set, Filigree transitions old-position findings for the same
rule/file that weren't in the latest scan to `unseen_in_latest`. Over repeated
runs these accumulate. Loomweave's REQ-FINDING-06 wants `loomweave analyze
--prune-unseen` to "remove `unseen_in_latest` findings older than 30 days
(configurable)." Loomweave has **no way to do this** — there is no
prune/delete/retention route on Filigree's side (confirmed: nothing in Loomweave's
`FiligreeHttpClient`, in the pinned `docs/federation/contracts.md`, or in the
Filigree MCP/HTTP tool surface). It is a server-side retention operation Loomweave
cannot implement alone.

## 2. Federation constraint (load-bearing — `weft.md` §3–§5)

- **Enrich-only.** Filigree's finding lifecycle must remain fully correct if
  Loomweave *never* calls prune. Prune is an optional retention convenience, never
  a required step. Introduce no coupling where Filigree depends on Loomweave
  calling it.
- **`scan_source`-scoped.** The operation must be scoped so Loomweave can only
  prune its own (`loomweave`) findings and can never affect Wardline's or any
  other tool's findings.

## 3. Primary ask — design and implement a prune surface

Resolve these and implement:

1. **Surface shape.** Pick one and justify:
   - (a) a dedicated route, e.g. `POST /api/v1/findings:prune` with body
     `{ "scan_source": "loomweave", "unseen_older_than_days": 30 }`;
   - (b) `DELETE /api/v1/findings?scan_source=loomweave&unseen_in_latest=true&older_than_days=30`;
   - (c) a field on the existing scan-results intake (e.g.
     `prune_unseen_older_than_days`) so prune piggybacks on the completing POST.

   (c) avoids a new route but conflates emit and retention; (a)/(b) are cleaner
   separations. Your call.
2. **Semantics — delete vs. archive.** REQ-FINDING-06 says "removes," but you own
   findings lifecycle: decide whether prune hard-deletes or soft-archives /
   dismisses (audit-preserving). Loomweave only needs *a* retention trigger; the
   durability/audit policy is yours.
3. **"Age" definition.** What timestamp gates the 30-day threshold — when the
   finding first transitioned to `unseen_in_latest`, or `updated_at` / last-seen?
   If there is no "became-unseen-at" timestamp today, that may need adding.
4. **Response shape.** Return counts (e.g.
   `{ "findings_pruned": N, "scan_source": "loomweave" }`) so Loomweave can log the
   outcome in `stats.json`.
5. **Auth.** Same posture as `/api/v1/scan-results`: `Authorization: Bearer
   <token>` + `x-filigree-actor: <actor>` headers.

## 4. Secondary ask — scan-run-create contract decision (Loomweave REQ-FINDING-05) — RESOLVED 2026-05-31

> **Resolved (2026-05-31): path (a) — tolerate-unknown is Filigree's permanent
> contract.** Filigree confirmed that accepting findings with a client-supplied
> `scan_run_id` it has never seen, ingesting them, and reconstructing the run in
> history is a stable, supported contract — not transitional leniency. There is
> no `POST /api/.../scan-runs` create endpoint (only read-only
> `GET /api/scan-runs` history), and none is planned. Loomweave adds **no Phase-0
> handshake**. The decision is pinned in Filigree's `contracts.md` §F6 and
> defended by `tests/api/test_files_api.py::TestUnknownScanRunIdContract`; the
> Loomweave-side caveats are dropped from `docs/federation/contracts.md`
> (scan-results intake) and `requirements.md` REQ-FINDING-05, which now record
> the three intake obligations (globally-unique UUIDv4 `run_id`, stable
> `scan_source`, benign-completion-warning handling). Tracked by
> clarion-694aab920a (closed). Original question retained below for context.

Loomweave currently POSTs findings with a `scan_run_id` that Filigree has never
seen; Filigree tolerates the unknown id, emits a warning, and proceeds. Loomweave
is deciding whether to add a Phase-0 "create the scan run first" handshake.

**Question for Filigree:** is "tolerate unknown `scan_run_id`, warn, proceed" the
intended *permanent* contract (Loomweave will then document that and stop treating
the warning as a gap), or do you want an explicit create endpoint (e.g.
`POST /api/v1/scan-runs` accepting a client-supplied id, or returning a
server-assigned one)? Answer this; only build the endpoint if you want it.

## 5. Deliverables / acceptance criteria

- The chosen prune surface implemented, with `scan_source` scoping enforced
  (a test proving a `loomweave`-scoped prune leaves `wardline` findings untouched).
- The wire request/response shape documented on Filigree's side **and** a
  normative fixture, so Loomweave can mirror it in `docs/federation/contracts.md`
  (matching how the scan-results intake and the `/api/v1/files` family are
  pinned there).
- A test that prune is enrich-only: deleting/archiving via prune doesn't corrupt
  the seen/unseen state of findings still present in the latest scan.
- A written answer to the scan-run-create question in §4.
