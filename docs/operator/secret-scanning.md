# Secret Scanning

Clarion scans source files before any file content can be used for LLM summaries. A detected credential creates a finding and marks entities from that file with `briefing_blocked: secret_present`. Structural analysis still runs, but summaries for that file do not.

## What Gets Blocked

Blocking is file-level. If `src/config.py` contains a detected key, entities from that file remain queryable through structural tools, but the `summary` tool returns a policy envelope instead of calling the LLM provider or writing `summary_cache`.

Plugin source files and `.env` sidecars are scanned. If a plugin reports an entity for some other in-project path that was not covered by the scanner, Clarion marks that entity `briefing_blocked: unscanned_source` so source bytes cannot reach the LLM provider without a prior scan.

## Whitelist A False Positive

Add `.clarion/secrets-baseline.yaml` and commit it with the source change:

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

The hash is SHA-1 over the matched literal bytes, matching `detect-secrets` v1.x baseline conventions. `justification` is required; entries without it are ignored and produce `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION`.

A matching baseline entry suppresses the block and records `CLA-INFRA-SECRET-BASELINE-MATCH` for audit.

## Override Flag

Use `--allow-unredacted-secrets` only when you deliberately accept that detected secrets may reach the LLM provider.

When detections exist, interactive runs prompt for:

```text
yes-i-understand
```

Non-TTY runs must pass both flags:

```bash
clarion analyze --allow-unredacted-secrets \
  --confirm-allow-unredacted-secrets=yes-i-understand .
```

Confirmed overrides do not set `briefing_blocked`; they emit `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` and add `secret_override_used` plus `secret_override_files_affected` to `runs.stats`. Passing `--allow-unredacted-secrets` on a clean repo is a no-op.

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
where rule_id like 'CLA-SEC-%'
   or rule_id like 'CLA-INFRA-SECRET-%';
```

Currently blocked entities:

```sql
select id, plugin_id, kind, name,
       json_extract(properties, '$.briefing_blocked') as block_reason
from entities
where json_extract(properties, '$.briefing_blocked') is not null;
```

Filigree integration for scanner findings is planned for v0.2. Until then, the local `findings` table is the authoritative WP5 audit surface.

## Limitations

The scanner is pattern-based. It can miss novel internal key formats and it can flag high-entropy test data. Use a justified baseline for reviewed false positives, and prefer `--no-llm` or an air-gapped workflow for repos where any source disclosure would be unacceptable.

Contextual credential suppression only recognises shell/Python `#` comments in v0.1. It does not recognise `//` or `/* */` comments; use a justified baseline entry for reviewed non-Python test fixtures.

See [ADR-013](../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) for design rationale.
