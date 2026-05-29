# Request to Filigree — `scan_source`-scoped finding retention/prune surface (+ scan-run-create contract decision)

> **⚠️ PRUNE ASK WITHDRAWN / SUPERSEDED (2026-05-30).** The prune surface this
> memo requested **already exists** in Filigree:
> `POST /api/loom/findings/clean-stale` with body
> `{"scan_source": "clarion", "older_than_days": 30, "actor": "…"}` →
> `{"findings_fixed": N, "scan_source": "…", "older_than_days": N}`. It
> soft-archives `unseen_in_latest` findings to `fixed` (auto-reopen on
> reappearance; Filigree ADR-015), `scan_source` required server-side as an
> accident-guard. Verified against Filigree's handler
> (`src/filigree/dashboard_routes/files.py`) and its API tests. Clarion's
> `--prune-unseen` now consumes it directly (REQ-FINDING-06 done); the contract
> is pinned in `docs/federation/contracts.md` → "Consumed Filigree route:
> clean-stale retention". My §1–§3 and §5 below (asking Filigree to *design and
> build* a prune surface) are therefore **moot** — the original "no prune route
> exists" premise was wrong. **§4 (scan-run-create contract decision) is the
> only live ask** and remains open.

**Status:** Prune ask withdrawn; §4 (scan-run-create) open (2026-05-30)
**Author side:** Clarion
**Tracking issue (Clarion):** §4 tracked under a `release:1.1` issue (Phase-0 scan-run-create handshake); the prune/`--prune-unseen` piece of `clarion-dd29e69e0e` is done.
**Sibling docs:**
- Clarion `docs/federation/contracts.md` → "Consumed Filigree route: clean-stale retention" (the route Clarion now consumes) and "…: scan-results intake".
- Clarion `docs/clarion/1.0/requirements.md` → REQ-FINDING-05, REQ-FINDING-06.
- Filigree intake handler for reference: `db.process_scan_results` (`db_files.py:857-926`, per ADR-014).

> **Note on placement.** This is a Clarion-authored *request to* Filigree, kept here for Clarion's reference. The authoritative Filigree-side artifact (design + implementation) should live in the Filigree repo; refresh `docs/federation/filigree-side/` with a mirror once Filigree drafts its response.

---

> The §1–§3 / §5 prune-design sections below are retained only as a record of
> the (mistaken) original request; see the withdrawal banner above. Skip to §4
> for the live question.

## 1. Problem in one paragraph

Clarion emits findings into Filigree via `POST /api/v1/scan-results` with
`scan_source: "clarion"`, `mark_unseen: true`, `complete_scan_run: true`. When
`mark_unseen` is set, Filigree transitions old-position findings for the same
rule/file that weren't in the latest scan to `unseen_in_latest`. Over repeated
runs these accumulate. Clarion's REQ-FINDING-06 wants `clarion analyze
--prune-unseen` to "remove `unseen_in_latest` findings older than 30 days
(configurable)." Clarion has **no way to do this** — there is no
prune/delete/retention route on Filigree's side (confirmed: nothing in Clarion's
`FiligreeHttpClient`, in the pinned `docs/federation/contracts.md`, or in the
Filigree MCP/HTTP tool surface). It is a server-side retention operation Clarion
cannot implement alone.

## 2. Federation constraint (load-bearing — `loom.md` §3–§5)

- **Enrich-only.** Filigree's finding lifecycle must remain fully correct if
  Clarion *never* calls prune. Prune is an optional retention convenience, never
  a required step. Introduce no coupling where Filigree depends on Clarion
  calling it.
- **`scan_source`-scoped.** The operation must be scoped so Clarion can only
  prune its own (`clarion`) findings and can never affect Wardline's or any
  other tool's findings.

## 3. Primary ask — design and implement a prune surface

Resolve these and implement:

1. **Surface shape.** Pick one and justify:
   - (a) a dedicated route, e.g. `POST /api/v1/findings:prune` with body
     `{ "scan_source": "clarion", "unseen_older_than_days": 30 }`;
   - (b) `DELETE /api/v1/findings?scan_source=clarion&unseen_in_latest=true&older_than_days=30`;
   - (c) a field on the existing scan-results intake (e.g.
     `prune_unseen_older_than_days`) so prune piggybacks on the completing POST.

   (c) avoids a new route but conflates emit and retention; (a)/(b) are cleaner
   separations. Your call.
2. **Semantics — delete vs. archive.** REQ-FINDING-06 says "removes," but you own
   findings lifecycle: decide whether prune hard-deletes or soft-archives /
   dismisses (audit-preserving). Clarion only needs *a* retention trigger; the
   durability/audit policy is yours.
3. **"Age" definition.** What timestamp gates the 30-day threshold — when the
   finding first transitioned to `unseen_in_latest`, or `updated_at` / last-seen?
   If there is no "became-unseen-at" timestamp today, that may need adding.
4. **Response shape.** Return counts (e.g.
   `{ "findings_pruned": N, "scan_source": "clarion" }`) so Clarion can log the
   outcome in `stats.json`.
5. **Auth.** Same posture as `/api/v1/scan-results`: `Authorization: Bearer
   <token>` + `x-filigree-actor: <actor>` headers.

## 4. Secondary ask — scan-run-create contract decision (Clarion REQ-FINDING-05)

Clarion currently POSTs findings with a `scan_run_id` that Filigree has never
seen; Filigree tolerates the unknown id, emits a warning, and proceeds. Clarion
is deciding whether to add a Phase-0 "create the scan run first" handshake.

**Question for Filigree:** is "tolerate unknown `scan_run_id`, warn, proceed" the
intended *permanent* contract (Clarion will then document that and stop treating
the warning as a gap), or do you want an explicit create endpoint (e.g.
`POST /api/v1/scan-runs` accepting a client-supplied id, or returning a
server-assigned one)? Answer this; only build the endpoint if you want it.

## 5. Deliverables / acceptance criteria

- The chosen prune surface implemented, with `scan_source` scoping enforced
  (a test proving a `clarion`-scoped prune leaves `wardline` findings untouched).
- The wire request/response shape documented on Filigree's side **and** a
  normative fixture, so Clarion can mirror it in `docs/federation/contracts.md`
  (matching how the scan-results intake and the `/api/v1/files` family are
  pinned there).
- A test that prune is enrich-only: deleting/archiving via prune doesn't corrupt
  the seen/unseen state of findings still present in the latest scan.
- A written answer to the scan-run-create question in §4.
