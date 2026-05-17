"""Frozen dataclasses for LLM and HTTP call audit data.

Replaces loose ``dict[str, Any]`` at 23+ construction sites with typed,
immutable value objects that produce hash-stable dicts via ``to_dict()``.
Follows the ``TokenUsage`` precedent (commit dffe74a6).

Trust-tier notes
----------------
* Construction — used by our code (Tier 1/2).
* ``to_dict()`` — serialization boundary, produces identical dicts to
  the old inline dict construction for hash stability.
* ``raw_response`` on ``LLMCallResponse`` is intentionally ``dict[str, Any]``
  because it's Tier 3 SDK data that varies across providers/versions.

Deep immutability
-----------------
All mutable containers (``dict``, ``list``) are converted to immutable
equivalents (``MappingProxyType``, ``tuple``) in ``__post_init__``.
``to_dict()`` methods convert back to plain ``dict``/``list`` for wire
format stability.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Any, Protocol, runtime_checkable

from elspeth.contracts.freeze import deep_freeze, deep_thaw, freeze_fields, require_int
from elspeth.contracts.token_usage import TokenUsage

# ---------------------------------------------------------------------------
# Call payload protocol — satisfied by all 6 DTOs via structural subtyping
# ---------------------------------------------------------------------------


@runtime_checkable
class CallPayload(Protocol):
    """Protocol for typed external call payload data.

    All frozen call-data dataclasses (LLMCallRequest, HTTPCallResponse, etc.)
    satisfy this protocol structurally — no explicit inheritance needed.
    ``RawCallPayload`` wraps pre-serialized dicts from ``PluginContext``.
    """

    def to_dict(self) -> dict[str, Any]: ...


@dataclass(frozen=True, slots=True)
class RawCallPayload:
    """Wrapper for pre-serialized call payload dicts from PluginContext.

    ``to_dict()`` returns a shallow copy to prevent callers from mutating
    the internal dict through the returned reference.
    """

    data: Mapping[str, Any]

    def __post_init__(self) -> None:
        freeze_fields(self, "data")

    def to_dict(self) -> dict[str, Any]:
        return {k: deep_thaw(v) for k, v in self.data.items()}


# ---------------------------------------------------------------------------
# LLM call data
# ---------------------------------------------------------------------------

_LLM_REQUEST_RESERVED_KEYS = frozenset(
    {"model", "messages", "temperature", "provider", "max_tokens"},
)


@dataclass(frozen=True, slots=True)
class LLMCallRequest:
    """Audit record for an outbound LLM API request."""

    model: str
    messages: Sequence[Mapping[str, Any]]
    temperature: float
    provider: str
    max_tokens: int | None = None
    extra_kwargs: Mapping[str, Any] = field(default_factory=lambda: MappingProxyType({}))

    def __post_init__(self) -> None:
        # Always deep-freeze inner message dicts — a pre-built tuple may
        # still contain mutable inner dicts (e.g. tuple([{"role": "user"}])).
        object.__setattr__(
            self,
            "messages",
            tuple(deep_freeze(m) for m in self.messages),
        )
        freeze_fields(self, "extra_kwargs")
        if collisions := (_LLM_REQUEST_RESERVED_KEYS & self.extra_kwargs.keys()):
            msg = f"extra_kwargs contains reserved key(s) that would overwrite audit fields: {collisions}"
            raise ValueError(msg)

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        Conditionally omits ``max_tokens`` when None, spreads
        ``extra_kwargs`` to match the old ``**kwargs`` pattern.
        """
        d: dict[str, Any] = {
            "model": self.model,
            "messages": [deep_thaw(m) for m in self.messages],
            "temperature": self.temperature,
            "provider": self.provider,
            **deep_thaw(self.extra_kwargs),
        }
        if self.max_tokens is not None:
            d["max_tokens"] = self.max_tokens
        return d


@dataclass(frozen=True, slots=True)
class LLMCallResponse:
    """Audit record for an LLM API response."""

    content: str
    model: str
    usage: TokenUsage
    raw_response: Mapping[str, Any]

    def __post_init__(self) -> None:
        freeze_fields(self, "raw_response")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        Calls ``self.usage.to_dict()`` for the usage field.
        """
        return {
            "content": self.content,
            "model": self.model,
            "usage": self.usage.to_dict(),
            "raw_response": deep_thaw(self.raw_response),
        }


@dataclass(frozen=True, slots=True)
class LLMCallError:
    """Audit record for an LLM API error."""

    type: str
    message: str
    retryable: bool

    def __post_init__(self) -> None:
        if not self.type:
            raise ValueError("LLMCallError.type must not be empty")
        if not self.message:
            raise ValueError("LLMCallError.message must not be empty")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        All fields always present (retryable is never optional).
        """
        return {
            "type": self.type,
            "message": self.message,
            "retryable": self.retryable,
        }


# ---------------------------------------------------------------------------
# HTTP call data
# ---------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class HTTPCallRequest:
    """Audit record for an outbound HTTP request.

    Handles three request shapes:

    * **Standard** (``resolved_ip`` is None): includes json/params by method.
    * **SSRF-safe** (``resolved_ip`` set, no hop): includes resolved_ip.
    * **Redirect hop** (``hop_number`` set): includes hop tracking fields.
      Successful hops include ``resolved_ip``; blocked pre-validation hops
      record ``redirect_from`` without fabricating a resolved IP.
    * **Audit-only metadata** (``audit_metadata`` set): includes request-linked
      provenance that was not sent over the wire.
    """

    method: str
    url: str
    headers: Mapping[str, str]
    json: Mapping[str, Any] | None = None
    params: Mapping[str, Any] | None = None
    audit_metadata: Mapping[str, Any] | None = None
    resolved_ip: str | None = None
    hop_number: int | None = None
    redirect_from: str | None = None

    def __post_init__(self) -> None:
        freeze_fields(self, "headers", "json", "params", "audit_metadata")
        if self.hop_number is not None and self.resolved_ip is None and self.redirect_from is None:
            msg = "hop_number without resolved_ip requires redirect_from for blocked redirect attempts"
            raise ValueError(msg)
        if self.redirect_from is not None and self.hop_number is None:
            msg = "redirect_from requires hop_number"
            raise ValueError(msg)

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        SSRF fields (resolved_ip, hop_number, redirect_from) are additive —
        they never suppress other fields.  json/params are serialized for
        all request shapes using hash-stability rules: POST always emits
        json (even None), GET always emits params (even None), other
        methods emit when non-None.
        """
        d: dict[str, Any] = {"method": self.method, "url": self.url}
        if self.resolved_ip is not None:
            d["resolved_ip"] = self.resolved_ip
        if self.hop_number is not None:
            d["hop_number"] = self.hop_number
        if self.redirect_from is not None:
            d["redirect_from"] = self.redirect_from
        d["headers"] = dict(self.headers)
        # POST always emits json (even None) and GET always emits params
        # (even None) for hash stability with existing audit records.
        # All other methods emit json/params when non-None — no silent drops.
        if self.json is not None or self.method == "POST":
            d["json"] = deep_thaw(self.json) if self.json is not None else None
        if self.params is not None or self.method == "GET":
            d["params"] = deep_thaw(self.params) if self.params is not None else None
        if self.audit_metadata is not None:
            d["audit_metadata"] = deep_thaw(self.audit_metadata)
        return d


@dataclass(frozen=True, slots=True)
class HTTPCallResponse:
    """Audit record for an HTTP response.

    Redirect hop responses omit ``body_size`` and ``body`` (only
    ``status_code`` and ``headers`` are meaningful for intermediate hops).
    """

    status_code: int
    headers: Mapping[str, str]
    body_size: int | None = None
    body: Mapping[str, Any] | tuple[Any, ...] | str | None = None
    redirect_count: int = 0

    def __post_init__(self) -> None:
        require_int(self.status_code, "status_code", min_value=100)
        require_int(self.body_size, "body_size", optional=True, min_value=0)
        require_int(self.redirect_count, "redirect_count", min_value=0)
        if self.body is not None and self.body_size is None:
            raise ValueError(
                "HTTPCallResponse.body requires body_size — without it, "
                "to_dict() silently drops body from the audit record. "
                "Set body_size=len(content) or omit body for redirect hops."
            )
        freeze_fields(self, "headers", "body")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        Includes ``body_size``/``body`` when ``body_size`` is not None.
        Includes ``redirect_count`` when > 0.
        """
        d: dict[str, Any] = {
            "status_code": self.status_code,
            "headers": dict(self.headers),
        }
        if self.body_size is not None:
            d["body_size"] = self.body_size
            if isinstance(self.body, (MappingProxyType, dict, tuple)):
                d["body"] = deep_thaw(self.body)
            else:
                d["body"] = self.body
        if self.redirect_count > 0:
            d["redirect_count"] = self.redirect_count
        return d


@dataclass(frozen=True, slots=True)
class HTTPCallError:
    """Audit record for an HTTP error.

    Network errors (timeout, connection refused) omit ``status_code``.
    HTTP errors (4xx/5xx) include ``status_code``.
    """

    type: str
    message: str
    status_code: int | None = None

    def __post_init__(self) -> None:
        if not self.type:
            raise ValueError("HTTPCallError.type must not be empty")
        if not self.message:
            raise ValueError("HTTPCallError.message must not be empty")

    def to_dict(self) -> dict[str, Any]:
        """Serialize to audit-trail dict.

        Includes ``status_code`` only when not None.
        """
        d: dict[str, Any] = {
            "type": self.type,
            "message": self.message,
        }
        if self.status_code is not None:
            d["status_code"] = self.status_code
        return d
