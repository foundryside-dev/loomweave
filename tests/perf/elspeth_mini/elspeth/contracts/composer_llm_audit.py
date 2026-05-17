"""Composer LLM-call audit primitives (L0).

Captures metadata for each outbound model call made by the web composer.
The record deliberately stores request/response metadata, integrity hashes,
and provider-supplied reasoning artifacts only when the provider returns them.
It never stores raw prompts, system prompt text, tool specs, or full provider
responses. Construction sites in L3 compute canonical hashes for the actual
messages/tools payloads sent to LiteLLM.

Layer: L0 (contracts). Imports nothing above contracts.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, fields
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal, Protocol

from elspeth.contracts.freeze import deep_thaw, freeze_fields, require_int

ComposerLLMProviderCostSource = Literal["not_available", "response_usage.cost"]

PROVIDER_COST_SOURCE_NOT_AVAILABLE: ComposerLLMProviderCostSource = "not_available"
PROVIDER_COST_SOURCE_RESPONSE_USAGE_COST: ComposerLLMProviderCostSource = "response_usage.cost"


class ComposerLLMCallStatus(StrEnum):
    """Outcome of one outbound composer model call."""

    SUCCESS = "success"
    TIMEOUT = "timeout"
    API_ERROR = "api_error"
    AUTH_ERROR = "auth_error"
    BAD_REQUEST_ERROR = "bad_request_error"
    MALFORMED_RESPONSE = "malformed_response"
    CANCELLED = "cancelled"


@dataclass(frozen=True, slots=True)
class ComposerLLMCall:
    """One outbound LLM call as recorded for composer audit.

    ``messages_hash`` is the canonical hash of the exact
    ``request.messages`` array sent to LiteLLM, including system prompt,
    accumulated history, assistant/tool-call turns, and the current user turn.
    It is an integrity check over the bytes sent to LiteLLM, not a stable
    identifier across deployments.

    ``tools_spec_hash`` is the canonical hash of the exact ``tools=[...]``
    specification sent to LiteLLM, or ``None`` when the call did not carry
    tools (for example diagnostics text generation).

    Cache-token fields (``cached_prompt_tokens``,
    ``cache_creation_input_tokens``, ``cache_read_input_tokens``) capture
    provider-reported prompt-cache statistics. They default to ``None``
    because most providers do not report cache metadata when caching is
    not active for the call. Per the CLAUDE.md fabrication policy, an
    absent cache field stays ``None`` rather than coerced to zero — an
    auditor can then distinguish "no cache reported" from "cache reported
    zero hits."

    ``reasoning_tokens`` and the reasoning artifact fields capture
    provider-reported reasoning metadata from APIs that expose it (for
    example OpenRouter ``message.reasoning`` / ``message.reasoning_details``
    and LiteLLM ``reasoning_content`` / ``thinking_blocks`` shapes). These
    fields are hidden audit data for operator debugging of tool-call and
    config failures; normal session-message APIs must not surface them as
    assistant chat content. Missing fields stay ``None`` rather than being
    fabricated.

    ``temperature`` and ``seed`` capture deterministic-sampling parameters
    set on composer LLM requests. Temperature is constant in the current
    implementation (``0.0``). Seed is ``42`` only for LiteLLM providers that
    advertise support for the OpenAI ``seed`` parameter, and ``None`` when
    omitted from the provider request. The audit row records the value
    actually sent so a reviewer can detect drift and correlate individual
    failures with the precise sampling regime that produced them. RGR
    investigation 2026-05-06 §4.4 traced ~33% hard-GREEN ceiling on the
    URL→download→line-explode scenario primarily to uncontrolled default
    sampling (~1.0) on the previous code path.
    """

    model_requested: str
    model_returned: str | None
    status: ComposerLLMCallStatus
    prompt_tokens: int | None
    completion_tokens: int | None
    total_tokens: int | None
    latency_ms: int
    provider_request_id: str | None
    messages_hash: str
    tools_spec_hash: str | None
    started_at: datetime
    finished_at: datetime
    error_class: str | None
    error_message: str | None
    temperature: float
    seed: int | None
    cached_prompt_tokens: int | None = None
    cache_creation_input_tokens: int | None = None
    cache_read_input_tokens: int | None = None
    reasoning_tokens: int | None = None
    reasoning_content: str | None = None
    reasoning_details: Any | None = None
    thinking_blocks: Any | None = None
    provider_cost: float | None = None
    provider_cost_source: ComposerLLMProviderCostSource = PROVIDER_COST_SOURCE_NOT_AVAILABLE

    def __post_init__(self) -> None:
        require_int(self.reasoning_tokens, "reasoning_tokens", optional=True, min_value=0)
        if self.reasoning_content is not None and type(self.reasoning_content) is not str:
            raise TypeError(f"reasoning_content must be str or None, got {type(self.reasoning_content).__name__}")
        freeze_fields(self, "reasoning_details", "thinking_blocks")
        if self.provider_cost is not None:
            if (
                type(self.provider_cost) is bool
                or type(self.provider_cost) not in (int, float)
                or not math.isfinite(float(self.provider_cost))
                or self.provider_cost < 0
            ):
                raise ValueError("provider_cost must be a finite non-negative number or None")
            object.__setattr__(self, "provider_cost", float(self.provider_cost))
        if self.provider_cost is None and self.provider_cost_source != PROVIDER_COST_SOURCE_NOT_AVAILABLE:
            raise ValueError("provider_cost_source must be not_available when provider_cost is None")
        if self.provider_cost is not None and self.provider_cost_source == PROVIDER_COST_SOURCE_NOT_AVAILABLE:
            raise ValueError("provider_cost_source must identify the provider metadata field when provider_cost is set")

    def to_dict(self) -> dict[str, Any]:
        """JSON-friendly dict for sidecar serialization."""
        raw = {field.name: deep_thaw(getattr(self, field.name)) for field in fields(self)}
        raw["status"] = self.status.value
        raw["started_at"] = self.started_at.isoformat()
        raw["finished_at"] = self.finished_at.isoformat()
        return raw


class ComposerLLMCallRecorder(Protocol):
    """Append-only sink for :class:`ComposerLLMCall` records."""

    def record_llm_call(self, call: ComposerLLMCall) -> None:
        """Persist or buffer one LLM call record."""
        ...

    def resolve_session(self, session_id: str) -> None:
        """Hint that the session_id is now resolved."""
        ...


# ---------------------------------------------------------------------------
# ComposerChatTurn — per-step-chat audit record (Phase A slice 5)
# ---------------------------------------------------------------------------


class ComposerChatTurnStatus(StrEnum):
    """Outcome of one per-step chat turn (Phase A slice 5).

    Mirrors :class:`ComposerLLMCallStatus` for symmetry, but a distinct
    enum because chat turns also have outcomes that don't apply to bare
    LLM calls (e.g. ``SYNTHETIC_UNAVAILABLE`` for the auto-drop synthetic
    message; the underlying LLM call may have ``TIMEOUT`` status but the
    chat turn surfaces a synthetic assistant message and is recorded as
    ``SYNTHETIC_UNAVAILABLE``). ``INVARIANT_VIOLATED`` records a defective
    model response that raised before an assistant chat message landed.
    """

    SUCCESS = "success"
    SYNTHETIC_UNAVAILABLE = "synthetic_unavailable"
    INVARIANT_VIOLATED = "invariant_violated"


class ComposerChatInitiator(StrEnum):
    """Who started the chat turn.

    ``user`` — the human typed a message into the chat input.
    ``step_entry_opener`` — a Phase A.5 proactive opener fired by the server
    when ``session.step`` changed.  Phase A only emits ``user``; the
    discriminator is wired now so the audit schema is stable across phases.
    """

    USER = "user"
    STEP_ENTRY_OPENER = "step_entry_opener"


@dataclass(frozen=True, slots=True)
class ComposerChatTurn:
    """One per-step chat turn as recorded for composer audit (Phase A slice 5).

    Sibling to :class:`ComposerLLMCall`.  Where ``ComposerLLMCall``
    records a *single outbound model request*, ``ComposerChatTurn``
    records the higher-level *conversational turn* — which step the user
    was on, who initiated it (user vs. proactive opener), the monotonic
    per-session sequence number, and integrity hashes for the messages
    exchanged.  An auditor can query "show me every chat turn at
    ``step=step_1_source`` for run X" without joining through
    ``ComposerLLMCall``.

    Phase A is advisory-only — no tool calls, no rejected tool calls.
    Phase B adds ``tool_calls`` / ``rejected_tool_calls`` fields.  Their
    absence in Phase A is recorded honestly (``tool_calls`` defaults to
    empty tuple), not fabricated.

    ``user_message_hash`` and ``assistant_message_hash`` are canonical
    stable hashes over the literal message strings.  Construction sites
    in L3 compute them via :func:`elspeth.core.canonical.stable_hash`;
    this record holds the hash only, never the raw text (the raw text is
    Tier-3 user input or LLM output and must not enter the audit row).

    ``latency_ms`` is end-to-end on the server: from user message
    accepted at the route to assistant message persisted in
    ``chat_history``.  On the synthetic-unavailable path it is still
    populated (the time spent waiting for the failing LLM call).
    """

    step: str
    initiator: ComposerChatInitiator
    chat_turn_seq: int
    user_message_hash: str
    assistant_message_hash: str
    latency_ms: int
    model: str
    status: ComposerChatTurnStatus
    started_at: datetime
    finished_at: datetime
    error_class: str | None = None

    def __post_init__(self) -> None:
        require_int(self.chat_turn_seq, "chat_turn_seq", min_value=0)
        require_int(self.latency_ms, "latency_ms", min_value=0)
        if type(self.initiator) is not ComposerChatInitiator:
            raise TypeError(f"initiator must be ComposerChatInitiator, got {type(self.initiator).__name__}")
        if type(self.status) is not ComposerChatTurnStatus:
            raise TypeError(f"status must be ComposerChatTurnStatus, got {type(self.status).__name__}")
        if self.status is ComposerChatTurnStatus.SUCCESS and self.error_class is not None:
            raise ValueError("error_class must be None when status is SUCCESS")
        failed_statuses = (
            ComposerChatTurnStatus.SYNTHETIC_UNAVAILABLE,
            ComposerChatTurnStatus.INVARIANT_VIOLATED,
        )
        if self.status in failed_statuses and self.error_class is None:
            raise ValueError(f"error_class must be populated when status is {self.status.value}")

    def to_dict(self) -> dict[str, Any]:
        """JSON-friendly dict for sidecar serialization (mirror of ComposerLLMCall.to_dict)."""
        raw = {field.name: deep_thaw(getattr(self, field.name)) for field in fields(self)}
        raw["status"] = self.status.value
        raw["initiator"] = self.initiator.value
        raw["started_at"] = self.started_at.isoformat()
        raw["finished_at"] = self.finished_at.isoformat()
        return raw


class ComposerChatTurnRecorder(Protocol):
    """Append-only sink for :class:`ComposerChatTurn` records (Phase A slice 5)."""

    def record_chat_turn(self, turn: ComposerChatTurn) -> None:
        """Persist or buffer one chat-turn record."""
        ...
