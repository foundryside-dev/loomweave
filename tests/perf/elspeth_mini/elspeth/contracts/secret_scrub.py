"""Secret-scrubbing for DeclarationContractViolation payloads (ADR-010 §Decision 3).

The Landscape audit trail is a legal record. Arbitrary Mapping[str, Any] payloads
(allowed by the DeclarationContractViolation signature) could carry API keys,
connection strings, or OAuth tokens from plugin ``config.options``. This helper
redacts values matching known secret patterns BEFORE the payload is handed to
``to_audit_dict``.

Coverage is best-effort: new secret formats need new patterns here. This is
the last line of defence, not the first — contract authors SHOULD structure
payloads so they never carry secrets (see per-contract TypedDict payload_schema).
"""

from __future__ import annotations

import re
from collections.abc import Mapping, Set
from typing import Any, cast
from urllib.parse import parse_qs, urlparse

from elspeth.contracts.url import SENSITIVE_PARAMS

_REDACTED = "<redacted-secret>"

# Heuristic patterns. Order matters — longer / more specific first.
#
# ADR-010 §Payload-schema enforcement H5 Layer 2 additions — closed-set blind spots:
#  - Azure SAS ``sig=`` parameter. The whole-string redaction rule means any
#    URI containing a SAS signature has the entire URI replaced — structure
#    (container name, blob path) would otherwise leak alongside the
#    authenticator.
#  - Database connection strings. Both ODBC-style (``Password=x;``) and
#    URL-style (``postgres(ql)?://u:p@h``, ``mysql://u:p@h``). The ODBC
#    match is case-insensitive because real-world conn strings mix case
#    (``PWD=``, ``password=``); the URL schemes are case-sensitive per RFC
#    3986 §3.1 so left as-is.
#  - Basic-auth URLs. The ``user:pass@`` discriminator is required so plain
#    HTTPS endpoints (Landscape resource URIs, example.com links) do not
#    get nuked in triage payloads — see ``test_plain_https_url_without
#    _credentials_passes_through``.
_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"AKIA[0-9A-Z]{16}"),  # AWS access key
    re.compile(r"sk-or-v1-[A-Za-z0-9_-]{20,}"),  # OpenRouter API key
    re.compile(r"sk-[A-Za-z0-9]{20,}"),  # OpenAI / generic "sk-" key
    re.compile(r"xox[abpr]-[A-Za-z0-9-]{10,}"),  # Slack token
    re.compile(r"ghp_[A-Za-z0-9]{36,}"),  # GitHub PAT
    re.compile(r"eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}"),  # JWT
    re.compile(r"-----BEGIN [A-Z ]+PRIVATE KEY-----"),  # PEM
    # Azure SAS signature — the ``sig=`` parameter value is the authenticator.
    re.compile(r"sig=[A-Za-z0-9%/+=]{20,}"),
    # ODBC-style conn-string password field; case-insensitive because
    # Password=, PASSWORD=, pwd=, PWD= all appear in the wild.
    re.compile(r"(?i)(?:password|pwd)=[^;\s]+"),
    # URL-style DB connection strings with embedded credentials.
    re.compile(r"postgres(?:ql)?://[^:/\s]+:[^@/\s]+@"),
    re.compile(r"mysql://[^:/\s]+:[^@/\s]+@"),
    re.compile(r"mongodb(?:\+srv)?://[^:/\s]+:[^@/\s]+@"),
    # Basic-auth HTTP(S) URLs — require the ``user:pass@`` discriminator so
    # credential-free endpoint URIs are NOT redacted.
    re.compile(r"https?://[^:/\s]+:[^@/\s]+@"),
)

# Key-name match is case-insensitive (see ``_scrub_value``). Every entry must
# be lowercase here; the lookup applies ``.lower()`` on the observed key.
#
# ADR-010 §Payload-schema enforcement H5 Layer 2 additions: bearer/session tokens
# carried under non-``authorization`` keys, and connection-string keys
# whose value may carry credentials even if the string itself doesn't
# happen to match a regex above.
_SECRET_KEY_NAMES: frozenset[str] = frozenset(
    {
        # Existing 2A set.
        "api_key",
        "apikey",
        "secret",
        "token",
        "password",
        "passwd",
        "authorization",
        # H5 additions — bearer / session / refresh families.
        "access_token",
        "refresh_token",
        "session_token",
        "auth_cookie",
        # H5 additions — connection / SAS families.
        "sas_token",
        "connection_string",
        "conn_string",
    }
)


def scrub_payload_for_audit(payload: Mapping[str, Any]) -> dict[str, Any]:
    """Return a deep-copied, secret-redacted version of ``payload``.

    - Key names matching ``_SECRET_KEY_NAMES`` (case-insensitive) are redacted
      regardless of value.
    - String values matching any pattern in ``_PATTERNS`` are replaced entirely.
    - Nested mappings and sequences are walked recursively.
    """
    # _scrub_value on a Mapping always returns a dict comprehension.
    # cast() tells mypy the return type without adding a runtime isinstance check.
    return cast(dict[str, Any], _scrub_value(payload, parent_key=None))


def scrub_text_for_audit(text: str) -> str:
    """Return a secret-redacted version of freeform audit text.

    Uses the same whole-string replacement rules as payload string values so
    persisted exception messages do not bypass the audit scrubber.
    """
    return cast(str, _scrub_value(text, parent_key=None))


def _scrub_value(value: Any, *, parent_key: str | None) -> Any:
    if parent_key is not None and parent_key.lower() in _SECRET_KEY_NAMES:
        return _REDACTED
    if isinstance(value, Mapping):
        return {k: _scrub_value(v, parent_key=k) for k, v in value.items()}
    if isinstance(value, str):
        if _contains_sensitive_http_url(value):
            return _REDACTED
        for pattern in _PATTERNS:
            if pattern.search(value):
                # Replace the whole string — partial redaction leaks structure.
                return _REDACTED
        return value
    if isinstance(value, (list, tuple)):
        return [_scrub_value(item, parent_key=None) for item in value]
    if isinstance(value, Set):
        return sorted(
            (_scrub_value(item, parent_key=None) for item in value),
            key=repr,
        )
    return value


def _contains_sensitive_http_url(value: str) -> bool:
    parsed = urlparse(value)
    if parsed.scheme.lower() not in {"http", "https"} or not parsed.netloc:
        return False
    if parsed.username is not None or parsed.password is not None:
        return True
    query_params = parse_qs(parsed.query, keep_blank_values=True)
    if any(_base_param_name(key.lower()) in SENSITIVE_PARAMS for key in query_params):
        return True
    fragment_params = parse_qs(parsed.fragment, keep_blank_values=True)
    return any(_base_param_name(key.lower()) in SENSITIVE_PARAMS for key in fragment_params)


def _base_param_name(key: str) -> str:
    bracket = key.find("[")
    if bracket != -1:
        key = key[:bracket]
    dot = key.find(".")
    if dot != -1:
        key = key[:dot]
    return key
