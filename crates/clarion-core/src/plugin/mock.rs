//! In-process mock plugin test harness.
//!
//! This module provides three canned plugin behaviours for use in unit tests
//! that exercise the transport and supervisor layers without spawning a real
//! subprocess:
//!
//! - [`MockPlugin::new_compliant`] — full JSON-RPC 2.0 handshake + `analyze_file`.
//! - [`MockPlugin::new_crashing`] — crashes (silently drops frames) after `initialized`.
//! - [`MockPlugin::new_oversize`] — responds to `initialize` with an oversized frame.
//!
//! # I/O model
//!
//! The mock exposes two I/O handles:
//! - [`MockPlugin::stdin`] — `&mut Vec<u8>` satisfying `impl Write`. The test
//!   (acting as the core) calls [`write_frame`] into this sink.
//! - [`MockPlugin::stdout`] — `&mut Cursor<Vec<u8>>` satisfying `impl BufRead`.
//!   The test calls [`read_frame`] from this source.
//!
//! After each batch of writes the test calls [`MockPlugin::tick`], which drains
//! the inbox, dispatches each frame according to `MockBehaviour`, and appends
//! response frames to the outbox. The test then reads from `stdout()`.
//!
//! # Oversize behaviour
//!
//! [`MockPlugin::new_oversize`] writes a frame whose `Content-Length` header
//! declares [`MOCK_OVERSIZE_BYTES`] (2 MiB) but whose actual body is only a
//! short placeholder. [`read_frame`]'s ceiling check fires before the body is
//! read, so the test does not need to allocate 2 MiB to trigger the error.

use std::io::Cursor;

use thiserror::Error;

use super::{
    AnalyzeFileResult, AnalyzeFileStats, InitializeResult, JsonRpcVersion, RequestEnvelope,
    ResponseEnvelope, ResponsePayload, ShutdownResult, read_frame, write_frame,
};
use crate::plugin::Frame;
use crate::plugin::limits::ContentLengthCeiling;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Content-Length value written by [`MockPlugin::new_oversize`].
///
/// 2 MiB — well above any realistic test ceiling (typically 64 KiB – 1 MiB).
/// The body bytes in the outbox are shorter; `read_frame`'s ceiling check fires
/// on the header value before the body is consumed.
pub const MOCK_OVERSIZE_BYTES: usize = 2 * 1024 * 1024;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors produced by [`MockPlugin::tick`].
#[derive(Debug, Error)]
pub enum MockError {
    /// Frame read/write failed.
    #[error("transport error: {0}")]
    Transport(#[from] super::TransportError),

    /// JSON serialisation/deserialisation failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// A message arrived that the mock's protocol state machine did not expect
    /// (e.g. an unknown method, or a message after the mock has exited).
    #[error("protocol state violation: {0}")]
    Protocol(String),
}

// ── State machine ─────────────────────────────────────────────────────────────

/// Internal lifecycle state of a [`MockPlugin`].
///
/// Transitions:
/// ```text
/// Fresh ──(initialize response sent)──► Initialized
/// Initialized ──(initialized notification received)──► Ready  [Compliant]
///                                                   ──► Crashed [Crashing]
/// Ready ──(shutdown response sent)──► ShutdownRequested
/// ShutdownRequested ──(exit notification received)──► Exited
/// * ──(exit notification received)──► Exited  [shortcut]
/// Crashed ── all further frames silently dropped
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
enum MockState {
    /// No frames exchanged yet.
    Fresh,
    /// `initialize` response has been sent; awaiting `initialized` notification.
    Initialized,
    /// `initialized` received; ready for `analyze_file`, `shutdown`, etc.
    Ready,
    /// Crashed after `initialized`; further frames are silently ignored.
    Crashed,
    /// `shutdown` response sent; awaiting `exit` notification.
    ShutdownRequested,
    /// `exit` received; no further frames are processed.
    Exited,
}

// ── Behaviour enum ────────────────────────────────────────────────────────────

/// Canned behaviour for a [`MockPlugin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockBehaviour {
    /// Responds to every request with a well-formed result.
    ///
    /// For `analyze_file`, returns one entity with:
    /// - `id`: `"mock:function:stub"`
    /// - `kind`: `"function"`
    /// - `qualified_name`: `"stub"`
    /// - `source.file_path`: the path configured via
    ///   [`MockPlugin::set_compliant_entity_path`] (defaults to `"/tmp/stub.mock"`
    ///   which will fail the jail check in tests that use a `TempDir` project root;
    ///   call the setter before `analyze_file` to supply an in-root path).
    Compliant,

    /// Responds to `initialize` normally; crashes after `initialized`.
    ///
    /// Any frame received after the `initialized` notification is silently
    /// dropped — no response is produced. Simulates a subprocess that exited
    /// post-handshake.
    Crashing,

    /// Responds to `initialize` with a frame whose `Content-Length` declares
    /// [`MOCK_OVERSIZE_BYTES`] but whose actual body is a short placeholder.
    ///
    /// `read_frame` with `max_bytes < MOCK_OVERSIZE_BYTES` returns
    /// [`TransportError::FrameTooLarge`] without reading the body.
    Oversize,

    /// Responds to `analyze_file` with one entity whose `kind` is `"unknown"`,
    /// which is not declared in the mock's manifest `entity_kinds`.
    ///
    /// Used by T3 to verify the host drops undeclared-kind entities.
    UndeclaredKind,

    /// Responds to `analyze_file` with one entity whose `id` field is
    /// `"mock:function:stub"` but whose `kind` is `"function"` and
    /// `qualified_name` is `"deliberately.wrong"` — so the expected id would
    /// be `"mock:function:deliberately.wrong"` while the returned id says
    /// `"mock:function:stub"`.
    ///
    /// Used by T4 to verify the host drops identity-mismatched entities.
    IdMismatch,

    /// Responds to `analyze_file` with `escape_count` entities each having a
    /// `source.file_path` of `"/tmp/escape_root_MOCK"` — a path that will
    /// canonicalise outside any `TempDir`-based project root.
    ///
    /// Single escape count (1) → T5 (drop-not-kill).
    /// Eleven or more → T6 (breaker trip).
    EscapingPath(usize),
}

// ── Mock plugin ───────────────────────────────────────────────────────────────

/// In-process mock plugin; stands in for a real subprocess during unit tests.
///
/// See the [module-level documentation](self) for the I/O model and usage.
pub struct MockPlugin {
    behaviour: MockBehaviour,
    state: MockState,
    /// Source path emitted in the `analyze_file` response entity for
    /// [`MockBehaviour::Compliant`]. Set via [`set_compliant_entity_path`](Self::set_compliant_entity_path).
    compliant_entity_path: String,
    /// Bytes the core has written via [`write_frame`]; the mock reads here on
    /// each [`tick`](Self::tick) call.
    inbox: Vec<u8>,
    /// Bytes the mock has produced; the core reads here via [`read_frame`].
    ///
    /// `Cursor<Vec<u8>>` tracks a read position independently of the vec's
    /// length, so appending to `outbox.get_mut()` does not disturb the
    /// position — the core sees new bytes immediately on the next `read_frame`
    /// call without a `set_position` reset.
    outbox: Cursor<Vec<u8>>,
}

impl MockPlugin {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Creates a compliant mock that fully implements the plugin protocol.
    pub fn new_compliant() -> Self {
        Self::new(MockBehaviour::Compliant)
    }

    /// Creates a mock that crashes after the `initialized` notification.
    pub fn new_crashing() -> Self {
        Self::new(MockBehaviour::Crashing)
    }

    /// Creates a mock that responds to `initialize` with an oversized frame.
    pub fn new_oversize() -> Self {
        Self::new(MockBehaviour::Oversize)
    }

    /// Creates a mock that responds to `analyze_file` with an entity whose
    /// `kind` is `"unknown"` — not in the manifest's `entity_kinds`.
    ///
    /// Used by T3 (ontology-boundary enforcement).
    pub fn new_undeclared_kind() -> Self {
        Self::new(MockBehaviour::UndeclaredKind)
    }

    /// Creates a mock that responds to `analyze_file` with an entity whose
    /// `id` field does not match `entity_id(plugin_id, kind, qualified_name)`.
    ///
    /// Used by T4 (identity-mismatch check).
    pub fn new_id_mismatch() -> Self {
        Self::new(MockBehaviour::IdMismatch)
    }

    /// Creates a mock that responds to `analyze_file` with `escape_count`
    /// entities each having a `source.file_path` that canonicalises outside
    /// the project root.
    ///
    /// Used by T5 (1 escape → drop-not-kill) and T6 (11 escapes → breaker trip).
    pub fn new_escaping_path(escape_count: usize) -> Self {
        Self::new(MockBehaviour::EscapingPath(escape_count))
    }

    fn new(behaviour: MockBehaviour) -> Self {
        Self {
            behaviour,
            state: MockState::Fresh,
            compliant_entity_path: "/tmp/stub.mock".to_owned(),
            inbox: Vec::new(),
            outbox: Cursor::new(Vec::new()),
        }
    }

    /// Override the `source.file_path` emitted by [`MockBehaviour::Compliant`]
    /// in `analyze_file` responses.
    ///
    /// The default is `"/tmp/stub.mock"`, which lies outside any `TempDir`-based
    /// project root and will fail the jail check. Call this after construction
    /// and before `analyze_file` to supply a path that exists inside the test's
    /// `project_root`.
    pub fn set_compliant_entity_path(&mut self, path: impl Into<String>) {
        self.compliant_entity_path = path.into();
    }

    // ── I/O handles ───────────────────────────────────────────────────────────

    /// Returns the inbox as a `&mut Vec<u8>`.
    ///
    /// `Vec<u8>` implements `Write`, so callers can pass this directly to
    /// [`write_frame`].
    pub fn stdin(&mut self) -> &mut Vec<u8> {
        &mut self.inbox
    }

    /// Returns the outbox cursor as a `&mut Cursor<Vec<u8>>`.
    ///
    /// `Cursor<Vec<u8>>` implements `BufRead`, so callers can pass this
    /// directly to [`read_frame`].
    pub fn stdout(&mut self) -> &mut Cursor<Vec<u8>> {
        &mut self.outbox
    }

    // ── Tick ──────────────────────────────────────────────────────────────────

    /// Drains the inbox, dispatches frames, and appends responses to the outbox.
    ///
    /// Call this after each batch of [`write_frame`] calls to the [`stdin`](Self::stdin)
    /// handle. Any leftover bytes that do not form a complete frame are kept in
    /// the inbox for the next tick.
    ///
    /// # Errors
    ///
    /// Returns [`MockError`] if a transport or serialisation error occurs.
    /// Protocol-state violations (unknown method, message after exit) return
    /// [`MockError::Protocol`].
    pub fn tick(&mut self) -> Result<(), MockError> {
        // Steal the inbox bytes so we can parse them without borrowing issues.
        let bytes = std::mem::take(&mut self.inbox);
        let mut cursor = Cursor::new(bytes);

        loop {
            // Peek at the remaining bytes to detect EOF without blocking.
            // `cursor.position()` returns `u64`; on test hosts this always fits
            // in `usize`, but we use `try_from` to satisfy clippy's
            // `cast_possible_truncation` lint (which targets 32-bit targets).
            let pos = usize::try_from(cursor.position())
                .expect("cursor position exceeds usize::MAX — impossible on any current target");
            if pos >= cursor.get_ref().len() {
                // All bytes consumed.
                break;
            }

            // Try to read one complete frame. A `TruncatedBody` or EOF error
            // means we have a partial frame — put the remaining bytes back and
            // wait for the next tick.
            let frame = match read_frame(&mut cursor, ContentLengthCeiling::unbounded()) {
                Ok(f) => f,
                Err(super::TransportError::TruncatedBody { .. }) => {
                    // Partial frame: put unconsumed bytes back into inbox.
                    let remaining = cursor.into_inner()[pos..].to_vec();
                    self.inbox = remaining;
                    return Ok(());
                }
                Err(super::TransportError::Io(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    // Header section incomplete — partial frame.
                    let remaining = cursor.into_inner()[pos..].to_vec();
                    self.inbox = remaining;
                    return Ok(());
                }
                Err(e) => return Err(MockError::Transport(e)),
            };

            if let Err(e) = self.dispatch(&frame) {
                // Preserve the failing frame and all subsequent bytes so that
                // the caller can inspect or retry. `pos` is the start of the
                // frame that failed dispatch — restore from there so the inbox
                // contains the full failing frame plus anything queued behind it.
                let inner = cursor.into_inner();
                self.inbox = inner[pos..].to_vec();
                return Err(e);
            }
        }

        Ok(())
    }

    // ── Internal dispatch ─────────────────────────────────────────────────────

    /// Dispatch one frame according to the current behaviour and state.
    fn dispatch(&mut self, frame: &Frame) -> Result<(), MockError> {
        // Exited mocks consume all frames silently.
        if self.state == MockState::Exited {
            return Ok(());
        }
        // Crashed mocks consume all frames silently.
        if self.state == MockState::Crashed {
            return Ok(());
        }

        // Peek at the raw JSON to distinguish request (has `id`) from
        // notification (no `id`). We do NOT use serde's typed envelopes for
        // the peek because `#[serde(deny_unknown_fields)]` is absent on those
        // types and we only need one field.
        let raw: serde_json::Value = serde_json::from_slice(&frame.body)?;

        let has_id = raw.get("id").is_some_and(|v| !v.is_null());
        let method = raw
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| MockError::Protocol("frame missing 'method' field".into()))?
            .to_owned();

        if has_id {
            // ── Request ───────────────────────────────────────────────────────
            let req: RequestEnvelope = serde_json::from_value(raw)?;
            self.handle_request(&req)?;
        } else {
            // ── Notification ──────────────────────────────────────────────────
            self.handle_notification(&method)?;
        }

        Ok(())
    }

    fn handle_request(&mut self, req: &RequestEnvelope) -> Result<(), MockError> {
        match req.method.as_str() {
            "initialize" => self.respond_initialize(req.id),
            "analyze_file" => self.respond_analyze_file(req.id),
            "shutdown" => self.respond_shutdown(req.id),
            other => Err(MockError::Protocol(format!(
                "unknown method {other:?} in state {:?}",
                self.state
            ))),
        }
    }

    fn handle_notification(&mut self, method: &str) -> Result<(), MockError> {
        match method {
            "initialized" => {
                // Transition state; no response produced.
                match self.state {
                    MockState::Initialized => match self.behaviour {
                        MockBehaviour::Crashing => {
                            self.state = MockState::Crashed;
                        }
                        _ => {
                            self.state = MockState::Ready;
                        }
                    },
                    _ => {
                        return Err(MockError::Protocol(format!(
                            "'initialized' notification received in unexpected state {:?}",
                            self.state
                        )));
                    }
                }
                Ok(())
            }
            "exit" => {
                // Accepted in any living state; transitions to Exited.
                self.state = MockState::Exited;
                Ok(())
            }
            other => Err(MockError::Protocol(format!(
                "unknown notification {other:?} in state {:?}",
                self.state
            ))),
        }
    }

    // ── Response builders ─────────────────────────────────────────────────────

    fn respond_initialize(&mut self, id: i64) -> Result<(), MockError> {
        if self.state != MockState::Fresh {
            return Err(MockError::Protocol(format!(
                "'initialize' received in unexpected state {:?}",
                self.state
            )));
        }
        if self.behaviour == MockBehaviour::Oversize {
            // Write a frame whose Content-Length declares MOCK_OVERSIZE_BYTES
            // but whose body is a short placeholder. The ceiling check in
            // read_frame fires before the body is consumed.
            let placeholder = b"{}";
            let header = format!("Content-Length: {MOCK_OVERSIZE_BYTES}\r\n\r\n");
            self.outbox.get_mut().extend_from_slice(header.as_bytes());
            self.outbox.get_mut().extend_from_slice(placeholder);
            // Stay in Fresh state — there is nothing more this mock will do.
            Ok(())
        } else {
            let result = InitializeResult {
                name: "mock-plugin".into(),
                version: "0.0.0".into(),
                ontology_version: "0.0.0".into(),
                capabilities: serde_json::json!({}),
            };
            let env = ResponseEnvelope {
                jsonrpc: JsonRpcVersion,
                id,
                payload: ResponsePayload::Result(serde_json::to_value(result)?),
            };
            self.write_response(&env)?;
            self.state = MockState::Initialized;
            Ok(())
        }
    }

    fn respond_analyze_file(&mut self, id: i64) -> Result<(), MockError> {
        if self.state != MockState::Ready {
            return Err(MockError::Protocol(format!(
                "'analyze_file' request in unexpected state {:?}",
                self.state
            )));
        }
        let entities = match &self.behaviour {
            MockBehaviour::UndeclaredKind => {
                // Entity with kind "unknown" — not in any compliant manifest's
                // entity_kinds list. Used by T3.
                vec![serde_json::json!({
                    "id": "mock:unknown:stub",
                    "kind": "unknown",
                    "qualified_name": "stub",
                    "source": { "file_path": "/tmp/mock_source.mock" }
                })]
            }
            MockBehaviour::IdMismatch => {
                // Entity with kind="function" and qualified_name="deliberately.wrong"
                // but id says "mock:function:stub" — mismatch. Used by T4.
                vec![serde_json::json!({
                    "id": "mock:function:stub",
                    "kind": "function",
                    "qualified_name": "deliberately.wrong",
                    "source": { "file_path": "/tmp/mock_source.mock" }
                })]
            }
            MockBehaviour::EscapingPath(escape_count) => {
                // `escape_count` entities each with a source.file_path pointing
                // outside any TempDir project root. The path must actually exist
                // on the filesystem for jail's canonicalize to resolve it (and
                // then find it outside the root). We use "/tmp" which exists on
                // all Linux systems. Each entity has a unique qualified_name so
                // identity checks pass; the id is constructed correctly.
                //
                // For the identity check to pass (so we get to the jail check),
                // the entity's id must equal entity_id("mock", "function", name).
                let count = *escape_count;
                (0..count)
                    .map(|i| {
                        let qname = format!("escape.entity{i}");
                        let eid = format!("mock:function:{qname}");
                        serde_json::json!({
                            "id": eid,
                            "kind": "function",
                            "qualified_name": qname,
                            "source": { "file_path": "/tmp" }
                        })
                    })
                    .collect()
            }
            _ => {
                // Compliant / Crashing / Oversize — emit a complete RawEntity so
                // it can survive the host's validation pipeline. The `id` must
                // equal entity_id("mock", "function", "stub") = "mock:function:stub".
                let path = self.compliant_entity_path.clone();
                vec![serde_json::json!({
                    "id": "mock:function:stub",
                    "kind": "function",
                    "qualified_name": "stub",
                    "source": { "file_path": path }
                })]
            }
        };
        let result = AnalyzeFileResult {
            entities,
            edges: vec![],
            stats: AnalyzeFileStats::default(),
        };
        let env = ResponseEnvelope {
            jsonrpc: JsonRpcVersion,
            id,
            payload: ResponsePayload::Result(serde_json::to_value(result)?),
        };
        self.write_response(&env)
    }

    fn respond_shutdown(&mut self, id: i64) -> Result<(), MockError> {
        if !matches!(self.state, MockState::Initialized | MockState::Ready) {
            return Err(MockError::Protocol(format!(
                "'shutdown' received in unexpected state {:?}",
                self.state
            )));
        }
        let result = ShutdownResult {};
        let env = ResponseEnvelope {
            jsonrpc: JsonRpcVersion,
            id,
            payload: ResponsePayload::Result(serde_json::to_value(result)?),
        };
        self.write_response(&env)?;
        self.state = MockState::ShutdownRequested;
        Ok(())
    }

    /// Serialise `env` and append it to the outbox as a framed message.
    fn write_response(&mut self, env: &ResponseEnvelope) -> Result<(), MockError> {
        let body = serde_json::to_vec(env)?;
        let frame = Frame { body };
        // Append to the outbox vec without disturbing the read position.
        let mut tmp: Vec<u8> = Vec::new();
        write_frame(&mut tmp, &frame)?;
        self.outbox.get_mut().extend_from_slice(&tmp);
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::protocol::{make_notification, make_request};
    use crate::plugin::{
        AnalyzeFileParams, InitializeParams, InitializedNotification, ResponsePayload,
        TransportError,
    };

    // ── Helper: write a framed envelope into the mock's stdin ─────────────────

    fn send_request<P: serde::Serialize>(mock: &mut MockPlugin, method: &str, params: &P, id: i64) {
        let env = make_request(method, params, id);
        let body = serde_json::to_vec(&env).expect("serialise");
        write_frame(mock.stdin(), &Frame { body }).expect("write_frame");
    }

    fn send_notification<P: serde::Serialize>(mock: &mut MockPlugin, method: &str, params: &P) {
        let env = make_notification(method, params);
        let body = serde_json::to_vec(&env).expect("serialise");
        write_frame(mock.stdin(), &Frame { body }).expect("write_frame");
    }

    // ── Mandatory test: compliant mock completes handshake ────────────────────

    /// Spec-mandated test (Task 3, §required test).
    ///
    /// Verifies that `MockPlugin::new_compliant()` can complete an `initialize`
    /// handshake through the real transport layer (`write_frame` / `read_frame`).
    #[test]
    fn compliant_mock_completes_handshake_through_transport() {
        let mut mock = MockPlugin::new_compliant();

        // Step 2+3: build initialize request and write it as a frame.
        let params = InitializeParams {
            protocol_version: "1.0".into(),
            project_root: "/tmp/x".to_owned(),
        };
        send_request(&mut mock, "initialize", &params, 1);

        // Step 4: tick so the mock processes the inbox and writes a response.
        mock.tick().expect("tick must succeed");

        // Step 5: read the response frame.
        let frame = read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024))
            .expect("read_frame must succeed");

        // Step 6: deserialise as ResponseEnvelope.
        let resp: ResponseEnvelope =
            serde_json::from_slice(&frame.body).expect("deserialise ResponseEnvelope");

        // Step 7: assert envelope fields.
        assert_eq!(resp.id, 1, "response id must echo request id");
        assert_eq!(resp.jsonrpc, JsonRpcVersion, "jsonrpc must be '2.0'");
        assert!(
            matches!(resp.payload, ResponsePayload::Result(_)),
            "payload must be Result, not Error; got {:?}",
            resp.payload
        );

        // Step 8: deserialise and assert InitializeResult fields.
        let result_val = match resp.payload {
            ResponsePayload::Result(v) => v,
            ResponsePayload::Error(e) => panic!("unexpected error payload: {e:?}"),
        };
        let init_result: InitializeResult =
            serde_json::from_value(result_val).expect("deserialise InitializeResult");
        assert_eq!(
            init_result.name, "mock-plugin",
            "name must be 'mock-plugin', got {:?}",
            init_result.name
        );
    }

    // ── Recommended test 1: compliant mock returns one entity on analyze_file ──

    #[test]
    fn compliant_mock_returns_one_entity_on_analyze_file() {
        let mut mock = MockPlugin::new_compliant();

        // Full handshake first.
        send_request(
            &mut mock,
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".into(),
                project_root: "/tmp/x".to_owned(),
            },
            1,
        );
        mock.tick().expect("tick after initialize");

        // Drain the initialize response frame so the cursor is ready for more.
        read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024))
            .expect("read initialize response");

        // Send initialized notification; mock transitions to Ready.
        send_notification(&mut mock, "initialized", &InitializedNotification {});
        mock.tick().expect("tick after initialized notification");

        // Send analyze_file request.
        send_request(
            &mut mock,
            "analyze_file",
            &AnalyzeFileParams {
                file_path: "src/lib.py".to_owned(),
            },
            2,
        );
        mock.tick().expect("tick after analyze_file");

        // Read the analyze_file response.
        let frame = read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024))
            .expect("read analyze_file response");
        let resp: ResponseEnvelope =
            serde_json::from_slice(&frame.body).expect("deserialise analyze_file ResponseEnvelope");

        assert_eq!(resp.id, 2);
        let result_val = match resp.payload {
            ResponsePayload::Result(v) => v,
            ResponsePayload::Error(e) => panic!("unexpected error: {e:?}"),
        };
        let result: crate::plugin::AnalyzeFileResult =
            serde_json::from_value(result_val).expect("deserialise AnalyzeFileResult");
        assert_eq!(
            result.entities.len(),
            1,
            "compliant mock must return exactly one entity; got {}",
            result.entities.len()
        );
    }

    // ── Recommended test 2: crashing mock produces no response after initialized

    #[test]
    fn crashing_mock_produces_no_response_after_initialized() {
        let mut mock = MockPlugin::new_crashing();

        // Handshake: initialize request.
        send_request(
            &mut mock,
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".into(),
                project_root: "/tmp/x".to_owned(),
            },
            1,
        );
        mock.tick().expect("tick after initialize");

        // Drain the initialize response.
        let frame = read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024))
            .expect("read initialize response");
        let resp: ResponseEnvelope = serde_json::from_slice(&frame.body).unwrap();
        assert!(matches!(resp.payload, ResponsePayload::Result(_)));

        // Record the outbox position after the initialize response; no new bytes
        // should appear after the crash.
        let pos_after_init = mock.stdout().position();

        // Send initialized notification — this triggers the crash transition.
        send_notification(&mut mock, "initialized", &InitializedNotification {});
        mock.tick().expect("tick after initialized notification");

        // Send analyze_file — should be silently dropped.
        send_request(
            &mut mock,
            "analyze_file",
            &AnalyzeFileParams {
                file_path: "src/lib.py".to_owned(),
            },
            2,
        );
        mock.tick()
            .expect("tick after analyze_file (crashing mock)");

        // The outbox must not have grown past the initialize response.
        let pos_after_crash = mock.stdout().position();
        let outbox_len = mock.stdout().get_ref().len() as u64;
        assert_eq!(
            outbox_len, pos_after_init,
            "crashing mock must not write any bytes after the initialize response; \
             outbox grew from {pos_after_init} to {outbox_len}"
        );
        // Read position should not have advanced either (no new frames produced).
        assert_eq!(
            pos_after_crash, pos_after_init,
            "cursor position must not advance past the initialize response"
        );
    }

    // ── Recommended test 3: oversize mock triggers FrameTooLarge ─────────────

    #[test]
    fn oversize_mock_triggers_frame_too_large() {
        let mut mock = MockPlugin::new_oversize();

        // Send initialize — the oversize mock will respond with a huge frame.
        send_request(
            &mut mock,
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".into(),
                project_root: "/tmp/x".to_owned(),
            },
            1,
        );
        mock.tick()
            .expect("tick must succeed even for oversize mock");

        // Read with a ceiling well below MOCK_OVERSIZE_BYTES.
        let ceiling = 64 * 1024; // 64 KiB
        let err = read_frame(mock.stdout(), ContentLengthCeiling::new(ceiling))
            .expect_err("read_frame must fail with FrameTooLarge");

        assert!(
            matches!(
                err,
                TransportError::FrameTooLarge { observed, ceiling: c }
                if observed == MOCK_OVERSIZE_BYTES && c == ceiling
            ),
            "expected FrameTooLarge {{ observed: {MOCK_OVERSIZE_BYTES}, ceiling: {ceiling} }}, got: {err}"
        );
    }

    // ── B3 test: double initialize is rejected ────────────────────────────────

    #[test]
    fn mock_rejects_double_initialize() {
        // Sending initialize twice must trigger MockError::Protocol on the
        // second tick because the mock is no longer in MockState::Fresh.
        let mut mock = MockPlugin::new_compliant();

        // First initialize — must succeed.
        send_request(
            &mut mock,
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".into(),
                project_root: "/tmp/x".to_owned(),
            },
            1,
        );
        mock.tick().expect("first initialize must succeed");
        // Drain the response.
        read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024))
            .expect("read first initialize response");

        // Second initialize — the mock is now Initialized, not Fresh.
        send_request(
            &mut mock,
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".into(),
                project_root: "/tmp/x".to_owned(),
            },
            2,
        );
        let err = mock.tick().expect_err("second initialize must fail");
        assert!(
            matches!(err, MockError::Protocol(ref msg) if msg.contains("Initialized")),
            "error must mention the unexpected state (Initialized); got: {err}"
        );
    }

    // ── B4 test: inbox preserved after dispatch error ─────────────────────────

    #[test]
    fn mock_tick_preserves_remaining_inbox_after_dispatch_error() {
        // Arrange: compliant mock. Enqueue TWO frames in one batch:
        //   frame 1 — valid initialize (will succeed, transitions to Initialized)
        //   frame 2 — second initialize (will fail B3 state guard)
        // tick() must return Err and the inbox must still hold frame 2's bytes.
        let mut mock = MockPlugin::new_compliant();

        // Build frame 1: valid initialize.
        let init_params = InitializeParams {
            protocol_version: "1.0".into(),
            project_root: "/tmp/x".to_owned(),
        };
        {
            use crate::plugin::protocol::make_request;
            let env = make_request("initialize", &init_params, 1);
            let body = serde_json::to_vec(&env).expect("serialise frame 1");
            write_frame(mock.stdin(), &Frame { body }).expect("write frame 1");
        }

        // Build frame 2: second initialize (will trigger state guard).
        {
            use crate::plugin::protocol::make_request;
            let env = make_request("initialize", &init_params, 2);
            let body = serde_json::to_vec(&env).expect("serialise frame 2");
            write_frame(mock.stdin(), &Frame { body }).expect("write frame 2");
        }

        // Record approximate minimum byte length of one framed initialize message
        // so we can assert the inbox is non-trivially non-empty.
        let approx_frame_min = 10; // conservative: even a tiny frame has at least header + body

        // tick() — frame 1 succeeds, frame 2 errors.
        let err = mock
            .tick()
            .expect_err("tick must error on double initialize");
        assert!(
            matches!(err, MockError::Protocol(_)),
            "expected Protocol error; got: {err}"
        );

        // Inbox must contain the remaining bytes (frame 2 was not dispatched but
        // its bytes should have been preserved by the B4 fix).
        // Note: after B3 state guard fires, the dispatch error returns before
        // frame 2 is written to the outbox, so only frame 1's response appears.
        assert!(
            mock.inbox.len() >= approx_frame_min,
            "inbox must still hold frame 2 bytes after dispatch error; \
             inbox.len() = {}",
            mock.inbox.len()
        );

        // Outbox must contain exactly one response frame (the successful first
        // initialize response). Read it to confirm.
        let frame = read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024))
            .expect("must be able to read the first initialize response from outbox");
        let resp: ResponseEnvelope =
            serde_json::from_slice(&frame.body).expect("deserialise first initialize response");
        assert_eq!(
            resp.id, 1,
            "outbox frame must be the first initialize response"
        );
        assert!(
            matches!(resp.payload, ResponsePayload::Result(_)),
            "first initialize response must be a Result"
        );

        // No second frame in the outbox.
        let no_frame = read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024));
        assert!(
            no_frame.is_err(),
            "outbox must contain exactly one frame, but a second was readable"
        );
    }
}
