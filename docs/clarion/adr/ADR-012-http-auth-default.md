# ADR-012: HTTP Read-API Authentication — Unix Domain Socket Default with Token Fallback

**Status**: Superseded for the ADR-014 registry-backend HTTP read API
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: `clarion serve` exposes the read API to sibling tools; v0.1 panel threat-model scored the original `auth: none` default at risk 9 (T-02)

## Summary

**2026-05-19 supersession note**: ADR-014 now owns the security posture for the
Clarion HTTP read API used by Filigree's `registry_backend: clarion` mode. That
surface is unauthenticated and loopback-only by default, refuses non-loopback
binds unless `serve.http.allow_non_loopback: true`, and relies on an
operator-managed authenticated reverse proxy or equivalent access-control layer
for non-loopback exposure. The UDS/token design below remains historical context
for the earlier broad v0.1 HTTP API proposal; it is not the implementation
contract for the ADR-014 federation read surface.

`clarion serve` defaults to **Unix domain socket** at `.clarion/socket` (mode 0600, owner = current UID). Filesystem permissions are the auth; no network bind, no Bearer tokens, no CAP_NET_BIND_SERVICE surface. When a UDS is unavailable (Windows, SSH-port-forwarded remote access, cross-UID-namespace containers), `serve.auth: token` falls back to TCP on `127.0.0.1:8765` + a Bearer token auto-minted at `.clarion/auth.token` (mode 0600). `serve.auth: none` remains configurable for operators who explicitly accept the unauthenticated-loopback posture, but now emits `CLA-INFRA-HTTP-AUTH-DISABLED` (severity ERROR) per serve startup and shows a persistent banner in logs. The default flip closes T-02 and structurally reduces T-05 (DNS rebinding) by eliminating the TCP bind in the primary path.

## Context

The original v0.1 design (system-design §9 "Token auth") defaulted `serve.auth` to `none` on loopback, treating 127.0.0.1 as a trust boundary. The 2026-04-17 panel threat-model review (`09-threat-model.md` §7, T-02) scored this at risk 9 with:

> Any local process reads source / writes observations; shared Docker / devcontainers invalidate "loopback = private."

The panel's "three non-negotiable v0.1 controls" (`09-threat-model.md` §12 recommendation 1) named the specific fix:

> Flip the HTTP API default to "authenticated or not listening." Either bind to a mode-0600 Unix-domain socket by default, or auto-mint a token on first `clarion serve`. `auth: none` on loopback is defended in §10 by naming "loopback is not a boundary"; the design should then not make `none` the default.

The scope-commitment memo adopted the flip (`v0.1-scope-commitments.md:199`, action item 5). This ADR records the specific choice — UDS over auto-minted token as the primary — and names the fallback triggers.

Two candidates existed. UDS (a) has the additional property of removing the TCP bind entirely, which structurally addresses T-05 (DNS rebinding) in the primary path. Token-on-127.0.0.1 (b) is more portable but still binds a TCP port, leaving the DNS-rebinding surface open (mitigated by `Host:`/`Origin:` checks but not eliminated). UDS is the stronger primary; token is the fallback for environments that can't use UDS.

## Decision

### Primary — Unix domain socket (`serve.auth: uds`, the default)

`clarion serve` binds `<project_root>/.clarion/socket` with filesystem mode 0600 and owner = the UID running `clarion serve`. The process owns the socket for its lifetime; on graceful shutdown the socket is unlinked.

- **Transport**: HTTP/1.1 over UDS. Server-side `axum` + `tokio::net::UnixListener` + `hyper::server`. No `Authorization` header required; no Bearer check on the HTTP layer.
- **Client side**: sibling tools (Wardline, local agents) connect via UDS using `hyper-unix-connector` or equivalent. Endpoint URL convention: the CLI and config accept `unix:///absolute/path/to/.clarion/socket`; the local fully-qualified path is used in sibling config rather than a pseudo-URL.
- **Auth by filesystem**: mode 0600 + owner-match means only the owning UID can connect. Shared-Docker and shared-dev-host scenarios (T-02's original concern) are closed because a different UID cannot open the socket at all.
- **CAP_NET_BIND_SERVICE equivalent**: there is no TCP listener. DNS rebinding (T-05) is structurally inapplicable. `Host:` / `Origin:` checks are unnecessary.
- **Discovery**: `<project_root>/.clarion/socket` is the canonical path; `clarion serve` documents it at startup. Sibling tools read `<project_root>/.clarion/config.json`'s `serve.socket_path` for explicit discovery.

### Fallback — TCP + Bearer token (`serve.auth: token`)

When UDS is unavailable or the operator explicitly opts in, Clarion falls back to TCP on `127.0.0.1:8765` with an auto-minted Bearer token.

- **Trigger (auto)**: Windows builds default to `serve.auth: token` since Windows UDS support is recent enough (Windows 10 1803+) that reliability varies. Clarion's Windows default is `token`.
- **Trigger (explicit)**: operators who need cross-UID-namespace access (e.g., local sibling running in a container, SSH port-forward exposing the serve endpoint to a remote workstation) set `serve.auth: token` in `clarion.yaml`.
- **Token auto-mint on first serve**: `.clarion/auth.token` written with mode 0600. Format: `clrn_` prefix + 32 bytes URL-safe base64 = 43 chars. No `clarion serve auth init` required to get started — first invocation auto-mints.
- **OS keychain promotion**: `clarion serve auth promote-to-keychain` moves the token from the file to the OS keychain (macOS Keychain / Linux libsecret / Windows Credential Manager) via the `keyring` crate. Promotes away from the filesystem hazard; emits `CLA-INFRA-TOKEN-STORAGE-DEGRADED` if the keychain is unavailable (falls back to file).
- **Wire**: `Authorization: Bearer clrn_<43chars>`. Constant-time server comparison.
- **Rotation**: `clarion serve auth rotate` with 24-hour grace window (both tokens accepted during the window).

### Explicit-none (`serve.auth: none`)

The operator can still disable auth. This is the escape hatch for air-gapped CI with external ingress control, for local debugging with trusted process landscape, or for operators who have a strong reason Clarion can't infer. It is loud rather than silent:

- `CLA-INFRA-HTTP-AUTH-DISABLED` (severity ERROR) emitted once per `clarion serve` startup with `serve.auth: none`. Propagated through the finding pipeline to Filigree; surfaces on the normal audit dashboard.
- Persistent banner at every serve-startup log message: `WARNING: clarion serve is running with NO authentication. Any local process can read the store.`
- The flag is named `--i-accept-no-auth` if operators want to enable it via CLI rather than `clarion.yaml` — the name is deliberately verbose.

### Wardline CI integration

`clarion check-auth --from wardline` remains the pre-flight check for CI. Its behaviour by mode:

- **UDS mode**: verifies the socket exists, is owned by the current UID, has mode 0600, and accepts a connection. Exit 0 = ready.
- **Token mode**: verifies `CLARION_TOKEN` env var or `.clarion/auth.token` is readable and authenticates against the running serve. Exit 0 = ready.
- **None mode**: exits 0 with warning (`CLA-INFRA-HTTP-AUTH-DISABLED` emitted at serve-side; check-auth confirms no auth is required).

### Cross-platform matrix

| Platform | Primary default | Fallback |
|---|---|---|
| Linux / macOS | UDS at `.clarion/socket` mode 0600 | TCP + token (explicit) |
| Windows 10 1803+ | TCP + token | (no further fallback; `none` is explicit opt-in) |
| WSL (Linux on Windows) | UDS | TCP + token (if WSL-Windows client access needed) |
| Remote-over-SSH (`ssh -L 8765:localhost:8765`) | TCP + token (explicit) | — |
| Container with separate UID namespace | TCP + token (explicit) | — |

## Alternatives Considered

### Alternative 1: Keep `auth: none` as default (status quo before this ADR)

**Pros**: zero configuration for local dev; no setup friction.

**Cons**: T-02 risk 9. Shared-Docker / devcontainer scenarios silently expose source to any local process. The panel called this out as one of three non-negotiable v0.1 controls. A security-analysis tool shipping with `auth: none` default fails credibility on day 1.

**Why rejected**: categorically incompatible with shipping as a security tool.

### Alternative 2: Auto-minted token as primary default (skip UDS)

Skip UDS; default to `serve.auth: token` on TCP + 127.0.0.1.

**Pros**: cross-platform identical (no Linux/Mac vs Windows split); easier to document.

**Cons**: leaves T-05 (DNS rebinding) open — TCP port binds are reachable via a browser tab on the host via crafted DNS responses. Mitigable with `Host:`/`Origin:` checks but not eliminated. UDS eliminates the attack surface structurally. For a security-conscious v0.1 release, the stronger primary is worth the platform asymmetry.

**Why rejected**: weaker primary when a stronger one is available at comparable implementation cost.

### Alternative 3: Mandatory TLS on the loopback interface

Bind HTTPS on 127.0.0.1 with a self-signed cert; operators trust the cert once.

**Pros**: standard HTTPS transport; familiar patterns.

**Cons**: cert-trust UX is famously terrible (browser warnings, keychain dialogs, curl `--insecure`); cert rotation adds operational surface; doesn't close T-05 (DNS rebinding attacks can still talk to the TLS endpoint if the attacker embeds the cert). Engineering cost for a mediocre security gain.

**Why rejected**: certificate management is worse UX than UDS/token; security gain is marginal.

### Alternative 4: Kernel-enforced namespace isolation (pid/net namespace)

Run `clarion serve` in a private network namespace reachable only from cooperating processes.

**Pros**: strongest isolation.

**Cons**: Linux-only; requires elevated privilege to create namespaces for non-root; hostile to every UX pattern (sibling tools would need to join the namespace). Engineering cost disproportionate to the threat.

**Why rejected**: cost-disproportionate; breaks the cross-sibling integration story.

### Alternative 5: Per-endpoint scoping (token with scope claims)

v0.1 tokens carry scope claims — read-only catalog, read-only findings, submit observations.

**Pros**: principle of least privilege at the endpoint level.

**Cons**: adds auth-framework complexity (JWT or similar) on the critical path. v0.1 has one project-wide read token; per-endpoint scoping is named as v0.2+ work (`detailed-design.md:1296`). Getting the basic auth shape right first is the larger win.

**Why rejected for v0.1**: premature. v0.2 adds per-endpoint scoping once the basic shape stabilises.

## Consequences

### Positive

- T-02 (risk 9) closed: default is no longer `auth: none`. UDS default + token fallback means the unauthenticated-by-default posture exists only when operators explicitly opt in via `serve.auth: none`.
- T-05 (DNS rebinding, risk 6) structurally closed in the primary path — no TCP bind means no browser-reachable endpoint.
- T-06 (MCP `auto_emit` bypass, risk 6) reduced: a local attacker would need socket access before the consent gate matters.
- The explicit-none path is loud. Operators bypassing auth see it in findings, logs, and compat reports. The bypass is not silent.
- Windows operators get the token path by default; no platform-specific UX degradation.
- CI integration via `clarion check-auth --from wardline` works uniformly across modes.

### Negative

- Platform asymmetry: UDS on Unix, TCP+token on Windows. Documentation must cover both; integration tests must cover both. Mitigation: the asymmetry is deliberate and named; Windows operators get the token path consistently.
- UDS support in HTTP client libraries varies. Wardline's HTTP client in v0.2 needs to support `hyper-unix-connector` or equivalent. Mitigation: the token fallback exists specifically for clients that can't do UDS. An early Wardline implementation can start on TCP+token and move to UDS later.
- Socket cleanup on crash: if `clarion serve` SIGKILL-dies, the `.clarion/socket` file persists. Next start needs to unlink stale sockets. Mitigation: startup logic unlinks existing sockets after verifying no process is listening.
- Cross-SSH remote access requires the token mode. Operators SSHing from a laptop to a dev VM cannot use UDS directly (unless they forward the socket via `ssh -o StreamLocalBindUnlink=yes -L /path/to/local.socket:/path/to/remote.socket`). Mitigation: documentation points at this path; `serve.auth: token` is the easier answer for most teams.

### Neutral

- `.clarion/socket` must be gitignored (it's a runtime artifact, not shared state). ADR-005 picks the exact ignore rules.
- `.clarion/auth.token` is also gitignored. Committing it is a leak of the token; ADR-005 covers this.
- The token format (`clrn_` + 32 bytes base64) stays as documented in detailed-design §7 — this ADR changes the default posture and the auto-mint trigger, not the token format.

## Related Decisions

- [ADR-005](./ADR-005-clarion-dir-tracking.md) — `.clarion/` git-committable by default, but `.clarion/socket` and `.clarion/auth.token` are runtime artifacts that must be excluded. ADR-005 picks the `.gitignore` rules that protect against both.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — closes the plugin-side attack surface (T-01, T-08, T-11, T-12); this ADR closes the HTTP-side attack surface (T-02, T-05). Together they are the panel's "three non-negotiable v0.1 controls" (third being ADR-013, secret scanner).

## References

- [Clarion v0.1 panel threat model T-02](../../implementation/v0.1-reviews/panel-2026-04-17/09-threat-model.md) (line 233) — original risk scoring.
- [Panel threat model §12 recommendation 1](../../implementation/v0.1-reviews/panel-2026-04-17/09-threat-model.md) (line 298) — the specific "authenticated or not listening" prescription.
- [Clarion v0.1 system design §10 "Loopback is not a security boundary"](../v0.1/system-design.md) — the paragraph this ADR flips; updated in the same commit.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — supersedes this ADR for the active registry-backend HTTP read API trust model.
- [Clarion v0.1 scope commitments — action 5](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md) (line 199) — the commitment mandate.
