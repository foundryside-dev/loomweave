# Secret Scanning

Loomweave scans source files before any file content can be used for LLM summaries. A detected credential creates a finding and marks entities from that file with `briefing_blocked: secret_present`. Structural analysis still runs, but summaries for that file do not.

## What Gets Blocked

Blocking is file-level. If `src/config.py` contains a detected key, entities from that file remain queryable through structural tools, but the `summary` tool returns a policy envelope instead of calling the LLM provider or writing `summary_cache`.

Plugin source files and `.env` sidecars are scanned. If a plugin reports an entity for some other in-project path that was not covered by the scanner, Loomweave marks that entity `briefing_blocked: unscanned_source` so source bytes cannot reach the LLM provider without a prior scan.

## Whitelist A False Positive

Add `.loomweave/secrets-baseline.yaml` and commit it with the source change:

```yaml
version: "1.0"
results:
  "src/auth/fixtures.py":
    - type: "AWS Access Key"
      hashed_secret: "25910f981e85ca04baf359199dd0bd4a3ae738b6"
      line_number: 42
      is_secret: false
      justification: "AWS documentation example key used in a test fixture."
```

The hash is SHA-1 over the matched literal bytes, matching `detect-secrets` v1.x baseline conventions. `justification` is required; entries without it are ignored and produce `LMWV-INFRA-SECRET-BASELINE-NO-JUSTIFICATION`.

A matching baseline entry suppresses the block and records `LMWV-INFRA-SECRET-BASELINE-MATCH` for audit.

## Override Flag

Use `--allow-unredacted-secrets` only when you deliberately accept that detected secrets may reach the LLM provider.

When detections exist, interactive runs prompt for:

```text
yes-i-understand
```

Non-TTY runs must pass both flags:

```bash
loomweave analyze --allow-unredacted-secrets \
  --confirm-allow-unredacted-secrets=yes-i-understand .
```

Confirmed overrides do not set `briefing_blocked`; they emit `LMWV-SEC-UNREDACTED-SECRETS-ALLOWED` and add `secret_override_used` plus `secret_override_files_affected` to `runs.stats`. Passing `--allow-unredacted-secrets` on a clean repo is a no-op.

## Exit Codes

| Code | Meaning |
|---|---|
| 0 | Analysis completed, with or without secret findings or a confirmed override. |
| 1 | Hard failure in the normal analysis path. |
| 78 | Secret override was requested but not confirmed; no run row is started. |

## Audit Trail

Local SQLite:

```sql
select rule_id, severity, message, evidence
from findings
where rule_id like 'LMWV-SEC-%'
   or rule_id like 'LMWV-INFRA-SECRET-%';
```

Currently blocked entities:

```sql
select id, plugin_id, kind, name,
       json_extract(properties, '$.briefing_blocked') as block_reason
from entities
where json_extract(properties, '$.briefing_blocked') is not null;
```

Filigree integration for scanner findings (WP9-B finding emission) is deferred to v1.1. Until then, the local `findings` table is the authoritative scanner audit surface.

## Limitations

The scanner is pattern-based. It can miss novel internal key formats and it can flag high-entropy test data. Use a justified baseline for reviewed false positives, and disable LLM dispatch entirely for repos where any source disclosure would be unacceptable — set `llm.allow_live_provider: false` in `loomweave.yaml` (or leave `LOOMWEAVE_LLM_LIVE` unset) so the recording provider is the only path Loomweave will take.

Contextual credential suppression currently recognises shell/Python `#` comments only. It does not recognise `//` or `/* */` comments; use a justified baseline entry for reviewed non-Python test fixtures.

See [ADR-013](../loomweave/adr/ADR-013-pre-ingest-secret-scanner.md) for design rationale.

## Trust assumption: loopback-no-token mode

The pre-ingest scanner's briefing-blocked annotations are only effective if
the HTTP read API also refuses to surface blocked entities to unauthorised
callers. The v1.0 HTTP API has one mode where it serves any local caller
without authentication: **loopback bind with no token configured.**

When both `serve.http.token_env` (legacy bearer) and `serve.http.identity_token_env`
(HMAC, preferred per [ADR-034](../loomweave/adr/ADR-034-federation-http-read-api-hardening.md))
are unset and the bind is loopback (the auto-selected per-project read-API port,
or an explicit loopback `serve.http.bind`; see ADR-044), the HTTP read
API serves unauthenticated. On a single-user developer workstation this is
the intended trust model: the loopback socket is reachable only from
processes on that host, and Loomweave's catalogue is no more sensitive than
the project source those processes can already read.

**On a multi-tenant developer host or shared CI runner the trust model is
different.** Any local process — any UID with read access to the loopback
bind socket — can read the entire non-blocked catalogue, including every
file's `entity_id`, `canonical_path`, `language`, and `content_hash`. This
is the documented v1.0 trust matrix; it is not a defect, but it is a
constraint operators must understand.

Multi-tenant operators MUST set `identity_token_env` (HMAC, preferred) or
`token_env` (bearer, legacy) before running `loomweave serve`. See
[`loomweave-http-read-api.md`](./loomweave-http-read-api.md) for the
configuration shape.

The Loomweave `serve` startup banner emits a `[TRUST]` line warning when
loopback-no-token mode is active: `HTTP API serving on loopback without
authentication; any local process on this host can read the catalogue.`
This warning is logged at `WARN` level at startup whenever both auth knobs
are unset and the bind is loopback.

## Pre-WP5 catalogue upgrade requirement

The WP5 pre-ingest secret scanner ships in v1.0. Briefing-blocked entities
are marked by writing `briefing_blocked: <reason>` into the file entity's
`properties` JSON column. v1.1 will promote `briefing_blocked` to a typed
column on `entities`; v1.0 carries it as a JSON property.

**A v1.0 binary opening a `.loomweave/loomweave.db` produced by a pre-WP5
Loomweave binary will find no `briefing_blocked` properties on any row.**
Pre-WP5 binaries never ran the scanner and never wrote the property; the
1.0 binary cannot retroactively discover which files contained secrets at
that earlier scan time. The HTTP read API will serve the entire catalogue
without refusal because every row's `briefing_blocked` is structurally
absent.

**Required upgrade procedure:** after installing the v1.0 binary against
a project root that was previously analyzed by a pre-WP5 binary, run
`loomweave analyze` (with the secret scanner active, which is the default)
against the project root **before** exposing the HTTP read API or calling
the `summary` MCP tool. The re-analyze produces a fresh briefing-blocked
annotation pass over all current file entities.

This applies only to upgrades from a Loomweave binary built before WP5
landed. A v1.0 installation that has never been opened by a pre-WP5
binary is unaffected.
