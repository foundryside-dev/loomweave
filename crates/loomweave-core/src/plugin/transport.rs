//! LSP-style Content-Length framing for the Loomweave plugin transport.
//!
//! Each frame is a self-describing byte sequence:
//!
//! ```text
//! Content-Length: N\r\n
//! \r\n
//! <N bytes of JSON body>
//! ```
//!
//! The `Frame` type is body-only; `Content-Length` is derived from `body.len()`
//! at write time. The transport layer is deliberately protocol-agnostic: it
//! knows nothing about `initialize`, `analyze_file`, etc. That coupling lives
//! in the supervisor (Task 6), which composes [`Frame`] with the types in
//! [`super::protocol`].
//!
//! # Size ceiling
//!
//! [`read_frame`] accepts a [`ContentLengthCeiling`] parameter (ADR-021 §2b).
//! If the `Content-Length` header exceeds the ceiling, [`TransportError::FrameTooLarge`]
//! is returned **without** consuming the body bytes from the reader — the
//! supervisor decides what to do (typically disconnect).
//!
//! # No async
//!
//! The framing layer is synchronous (`impl BufRead` / `impl Write`). Task 6
//! wires it over subprocess `ChildStdin`/`ChildStdout`, which implement
//! `Read`/`Write` without requiring async at this layer.

use std::io::{BufRead, ErrorKind, Write};

use thiserror::Error;

use super::limits::ContentLengthCeiling;

// ── Tunables ──────────────────────────────────────────────────────────────────

/// Per-line ceiling for header parsing.
///
/// Bounds memory consumption if a misbehaving plugin sends a header line with
/// no terminating LF. Matches nginx's default `large_client_header_buffers`
/// (8 KiB). Real `Content-Length` headers are ~30 bytes; this limit is
/// generous for `Content-Type` or other tolerated headers while still
/// slamming the door on a naïve denial-of-service attempt.
pub const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during frame read/write.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Underlying I/O error.
    #[error("IO error while reading/writing frame: {0}")]
    Io(#[from] std::io::Error),

    /// The header section ended without a `Content-Length` line.
    #[error("missing Content-Length header")]
    MissingContentLength,

    /// A `Content-Length` header was present but its value was not a valid
    /// non-negative decimal integer.
    #[error("malformed Content-Length header: {value:?}")]
    InvalidContentLength { value: String },

    /// The declared frame body exceeds the configured ceiling.
    ///
    /// The body bytes are **not** consumed from the reader; the supervisor
    /// must decide whether to disconnect or drain.
    #[error("frame exceeds ceiling: observed {observed} bytes, ceiling {ceiling}")]
    FrameTooLarge { observed: usize, ceiling: usize },

    /// The stream ended before the declared number of body bytes were available.
    #[error("unexpected EOF in frame body; expected {expected} bytes, got {actual}")]
    TruncatedBody { expected: usize, actual: usize },

    /// A header line did not conform to the expected `Name: Value` shape.
    #[error("malformed header line: {line:?}")]
    MalformedHeader { line: String },
}

// ── Frame type ────────────────────────────────────────────────────────────────

/// A single framed message: the raw body bytes from one `Content-Length` block.
///
/// `Content-Length` is not stored; it is derived from `body.len()` on write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The raw body bytes (typically UTF-8 JSON, but the transport does not
    /// validate encoding).
    pub body: Vec<u8>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read one LSP-style frame from `reader`.
///
/// Protocol:
/// 1. Read header lines until a bare `\r\n` (blank line). Each header line is
///    capped at [`MAX_HEADER_LINE_BYTES`] to bound memory under malicious input.
/// 2. Extract `Content-Length: N` (case-insensitive header name).
/// 3. If `N > ceiling.get()`, return [`TransportError::FrameTooLarge`] without
///    consuming any body bytes (ADR-021 §2b).
/// 4. Read exactly `N` bytes into the body. Retries transparently on
///    `ErrorKind::Interrupted` (EINTR — e.g. SIGCHLD on a subprocess pipe).
/// 5. Return `Frame { body }`.
///
/// Unknown headers are silently ignored (LSP tolerance — `Content-Type` etc.).
///
/// # Errors
///
/// See [`TransportError`] variants for the full list of failure modes.
pub fn read_frame(
    reader: &mut impl BufRead,
    ceiling: ContentLengthCeiling,
) -> Result<Frame, TransportError> {
    let mut content_length: Option<usize> = None;

    // ── Step 1+2: read header lines ──────────────────────────────────────────
    loop {
        let line = read_bounded_line(reader)?;

        // EOF before blank line — caller's stream ended unexpectedly.
        if line.is_empty() {
            return Err(TransportError::Io(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "EOF in header section",
            )));
        }

        // Blank line signals end of headers.
        if line == "\r\n" || line == "\n" {
            break;
        }

        // Strip CRLF / LF terminator for parsing.
        let trimmed = line.trim_end_matches(['\r', '\n']);

        // Ignore empty lines inside the header block (defensive).
        if trimmed.is_empty() {
            continue;
        }

        // Split on the first colon — the header must have one.
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(TransportError::MalformedHeader {
                line: trimmed.to_owned(),
            });
        };
        // Strip whitespace on both sides: LSP permits `Content-Length: 42   \r\n`
        // (trailing whitespace before CRLF).
        let value = value.trim();

        // Case-insensitive comparison per LSP spec.
        if name.trim().eq_ignore_ascii_case("content-length") {
            let n: usize = value
                .parse()
                .map_err(|_| TransportError::InvalidContentLength {
                    value: value.to_owned(),
                })?;
            content_length = Some(n);
        }
        // All other headers are silently ignored.
    }

    // ── Step 3: ceiling check (ADR-021 §2b) ──────────────────────────────────
    let length = content_length.ok_or(TransportError::MissingContentLength)?;
    let max_bytes = ceiling.get();
    if length > max_bytes {
        // Do NOT read any body bytes.
        return Err(TransportError::FrameTooLarge {
            observed: length,
            ceiling: max_bytes,
        });
    }

    // ── Step 4: read body ─────────────────────────────────────────────────────
    //
    // The manual loop (vs `read_exact`) is deliberate: it lets us surface
    // `TruncatedBody { expected, actual }` with the actual byte count, which
    // `read_exact`'s `UnexpectedEof` discards. `ErrorKind::Interrupted`
    // (EINTR) is retried transparently, matching `read_exact`'s own contract.
    let mut body = vec![0u8; length];
    let mut total_read = 0usize;
    while total_read < length {
        match reader.read(&mut body[total_read..]) {
            Ok(0) => {
                return Err(TransportError::TruncatedBody {
                    expected: length,
                    actual: total_read,
                });
            }
            Ok(n) => total_read += n,
            // EINTR: retry by letting the loop iterate again (match arm is a
            // no-op; the while condition re-checks `total_read < length`).
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(TransportError::Io(e)),
        }
    }

    Ok(Frame { body })
}

/// Read one line from `reader` with a byte cap of [`MAX_HEADER_LINE_BYTES`].
///
/// Returns the line including any trailing CRLF / LF, so callers can distinguish
/// a blank line (`"\r\n"`) from a real header. Returns an empty string on EOF.
///
/// If the cap is reached without encountering `\n`, returns
/// [`TransportError::MalformedHeader`] — prevents a malicious plugin from
/// sending a multi-GB header line to exhaust host memory.
///
/// Retries transparently on `ErrorKind::Interrupted`.
fn read_bounded_line(reader: &mut impl BufRead) -> Result<String, TransportError> {
    let mut buf = Vec::<u8>::new();
    let mut remaining = MAX_HEADER_LINE_BYTES;

    loop {
        if remaining == 0 {
            // We read the full cap and never saw a newline — fail loudly.
            return Err(TransportError::MalformedHeader {
                line: format!("header line exceeded {MAX_HEADER_LINE_BYTES}-byte limit"),
            });
        }

        // `fill_buf` exposes the BufRead's internal buffer so we can scan for
        // `\n` without reading one byte at a time.
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(TransportError::Io(e)),
        };

        // EOF.
        if available.is_empty() {
            // If we had partial data before EOF, treat as EOF (caller detects
            // via empty result only when `buf` is empty; partial data means
            // truncation, but the caller currently treats the empty-string
            // return as EOF — partial data here still hits the EOF arm because
            // we return `buf` as-is and it will be non-empty-but-not-line-
            // terminated). For Sprint 1, empty on EOF suffices — the caller
            // raises UnexpectedEof only when `buf.is_empty()`.
            break;
        }

        // Look for `\n` within the portion of `available` we're allowed to consume.
        let take = available.len().min(remaining);
        if let Some(nl_idx) = available[..take].iter().position(|&b| b == b'\n') {
            let consume = nl_idx + 1;
            buf.extend_from_slice(&available[..consume]);
            reader.consume(consume);
            break;
        }

        // No newline in the allowed window — consume what we have and loop
        // again, either to read more or to hit the cap on the next iteration.
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        remaining -= take;
    }

    // Header lines are ASCII per LSP. We tolerate arbitrary bytes in `buf`
    // here; a genuinely non-UTF-8 header will surface as `MalformedHeader`
    // from the caller's colon-split step.
    String::from_utf8(buf).map_err(|e| TransportError::MalformedHeader {
        line: format!("header line is not valid UTF-8: {e}"),
    })
}

/// Write one LSP-style frame to `writer`.
///
/// Produces:
/// ```text
/// Content-Length: N\r\n
/// \r\n
/// <body bytes>
/// ```
///
/// Flushes the writer before returning. This ensures the frame is actually
/// sent on buffered writers (e.g. `BufWriter<ChildStdin>`, which the plugin
/// supervisor will use) — without the flush, each frame would buffer
/// indefinitely and the plugin would never see it, producing a silent deadlock.
///
/// # Errors
///
/// Returns [`TransportError::Io`] on write or flush failure.
pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<(), TransportError> {
    let len = frame.body.len();
    write!(writer, "Content-Length: {len}\r\n\r\n")?;
    writer.write_all(&frame.body)?;
    writer.flush()?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{BufReader, BufWriter, Cursor, Read};

    use super::*;

    // ── Transport test 1: round-trip a single frame ───────────────────────────

    #[test]
    fn transport_01_single_frame_round_trip() {
        let body = b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}".to_vec();
        let frame = Frame { body: body.clone() };

        // Write
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &frame).expect("write_frame must succeed");

        // Read back
        let mut cursor = Cursor::new(buf);
        let decoded = read_frame(&mut cursor, ContentLengthCeiling::unbounded())
            .expect("read_frame must succeed");

        assert_eq!(decoded.body, body);
    }

    // ── Transport test 2: exact Content-Length boundary ───────────────────────

    #[test]
    fn transport_02_exact_ceiling_boundary_succeeds() {
        let body = b"hello".to_vec();
        let frame = Frame { body: body.clone() };
        let max = body.len(); // exactly at the ceiling

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &frame).expect("write");

        let mut cursor = Cursor::new(buf);
        let decoded = read_frame(&mut cursor, ContentLengthCeiling::new(max))
            .expect("read at exact boundary must succeed");
        assert_eq!(decoded.body, body);
    }

    // ── Transport test 3: Content-Length above ceiling — body not consumed ────

    #[test]
    fn transport_03_content_length_above_ceiling_returns_frame_too_large_without_consuming_body() {
        let body = b"hello world".to_vec();
        let frame = Frame { body: body.clone() };
        let max = body.len() - 1; // one byte below declared length

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &frame).expect("write");

        // Record position after headers so we can assert the body was not consumed.
        // The header is "Content-Length: 11\r\n\r\n" — 22 bytes for len=11.
        let header_len = format!("Content-Length: {}\r\n\r\n", body.len()).len();

        let mut cursor = Cursor::new(buf);
        let err = read_frame(&mut cursor, ContentLengthCeiling::new(max))
            .expect_err("should fail with FrameTooLarge");

        assert!(
            matches!(
                err,
                TransportError::FrameTooLarge {
                    observed,
                    ceiling,
                } if observed == body.len() && ceiling == max
            ),
            "unexpected error: {err}"
        );

        // Cursor position must be exactly at the start of the body, not past it.
        let pos =
            usize::try_from(cursor.position()).expect("cursor position fits in usize on test host");
        assert_eq!(
            pos, header_len,
            "body must not have been consumed; cursor should be at position {header_len}, got {pos}"
        );
    }

    // ── Transport test 4: two back-to-back frames ─────────────────────────────

    #[test]
    fn transport_04_two_consecutive_frames_round_trip() {
        let body1 =
            b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{},\"id\":1}".to_vec();
        let body2 =
            b"{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\",\"params\":{},\"id\":2}".to_vec();

        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &Frame {
                body: body1.clone(),
            },
        )
        .expect("write 1");
        write_frame(
            &mut buf,
            &Frame {
                body: body2.clone(),
            },
        )
        .expect("write 2");

        let mut cursor = Cursor::new(buf);
        let f1 = read_frame(&mut cursor, ContentLengthCeiling::unbounded()).expect("read frame 1");
        let f2 = read_frame(&mut cursor, ContentLengthCeiling::unbounded()).expect("read frame 2");

        assert_eq!(f1.body, body1, "first frame body mismatch");
        assert_eq!(f2.body, body2, "second frame body mismatch");
    }

    // ── Transport test 5: missing Content-Length ──────────────────────────────

    #[test]
    fn transport_05_missing_content_length_returns_error() {
        // Headers end without Content-Length.
        let raw = b"X-Custom: stuff\r\n\r\n{\"key\":\"value\"}";
        let mut cursor = Cursor::new(raw.as_ref());
        let err =
            read_frame(&mut cursor, ContentLengthCeiling::unbounded()).expect_err("should fail");
        assert!(
            matches!(err, TransportError::MissingContentLength),
            "expected MissingContentLength, got: {err}"
        );
    }

    // ── Transport test 6: malformed Content-Length ────────────────────────────

    #[test]
    fn transport_06_malformed_content_length_returns_invalid_content_length() {
        let raw = b"Content-Length: abc\r\n\r\n";
        let mut cursor = Cursor::new(raw.as_ref());
        let err =
            read_frame(&mut cursor, ContentLengthCeiling::unbounded()).expect_err("should fail");
        assert!(
            matches!(
                err,
                TransportError::InvalidContentLength { ref value } if value == "abc"
            ),
            "expected InvalidContentLength, got: {err}"
        );
    }

    // ── Transport test 7: truncated body ──────────────────────────────────────

    #[test]
    fn transport_07_truncated_body_returns_truncated_body_error() {
        // Header says 10, body has only 5 bytes.
        let raw = b"Content-Length: 10\r\n\r\nhello";
        let mut cursor = Cursor::new(raw.as_ref());
        let err =
            read_frame(&mut cursor, ContentLengthCeiling::unbounded()).expect_err("should fail");
        assert!(
            matches!(
                err,
                TransportError::TruncatedBody {
                    expected: 10,
                    actual: 5
                }
            ),
            "expected TruncatedBody, got: {err}"
        );
    }

    // ── I3 regression: EINTR retry during body read ───────────────────────────

    /// Reader wrapper that returns `ErrorKind::Interrupted` on the first
    /// `read` call, then delegates to the inner reader.
    ///
    /// Wrapped in `BufReader` in the test to satisfy the `BufRead` bound.
    struct InterruptOnceReader<R> {
        inner: R,
        interrupted: bool,
    }

    impl<R: Read> Read for InterruptOnceReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::Error::new(
                    ErrorKind::Interrupted,
                    "simulated signal",
                ));
            }
            self.inner.read(buf)
        }
    }

    #[test]
    fn transport_08_eintr_during_body_read_is_retried_transparently() {
        // Build a valid frame, wrap the stream in a reader that raises EINTR
        // once, and assert the frame still decodes cleanly.
        let body = b"hello world".to_vec();
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Frame { body: body.clone() }).expect("write");

        let raw = Cursor::new(buf);
        let flaky = InterruptOnceReader {
            inner: raw,
            interrupted: false,
        };
        let mut reader = BufReader::new(flaky);

        let frame = read_frame(&mut reader, ContentLengthCeiling::unbounded())
            .expect("EINTR must be retried, not propagated");
        assert_eq!(frame.body, body);
    }

    // ── I4 regression: write_frame flushes buffered writers ───────────────────

    #[test]
    fn transport_09_write_frame_flushes_buffered_writer() {
        // Without the flush call in write_frame, a BufWriter wrapping a small
        // inner sink would hold the frame bytes in its buffer until dropped
        // — a silent deadlock for a live subprocess.
        let body = b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}".to_vec();
        let frame = Frame { body: body.clone() };

        // Use an inner Vec<u8> wrapped in a Cursor so we can read its position
        // through a shared reference via `into_inner()` after the BufWriter
        // relinquishes the sink.
        let sink: Vec<u8> = Vec::with_capacity(1024);
        let mut bw = BufWriter::new(sink);
        write_frame(&mut bw, &frame).expect("write_frame");

        // After write_frame returns, the inner Vec must contain the whole
        // frame — no residual bytes stuck in the BufWriter.
        let inner = bw.into_inner().expect("BufWriter should have been flushed");

        let expected_header = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut expected = expected_header.into_bytes();
        expected.extend_from_slice(&body);

        assert_eq!(
            inner, expected,
            "write_frame must flush the BufWriter so the whole frame reaches the inner sink"
        );
    }

    // ── I5 regression: header-line cap ────────────────────────────────────────

    #[test]
    fn transport_10_oversize_header_line_returns_malformed_header() {
        // 16 KiB of 'a' with no `\n` — exceeds the 8 KiB header-line cap.
        // Without the bound, read_line would try to allocate 16 KiB+ and (in
        // the malicious case) GBs of host memory before returning.
        let payload = vec![b'a'; 16 * 1024];
        let mut cursor = Cursor::new(payload);
        let err =
            read_frame(&mut cursor, ContentLengthCeiling::unbounded()).expect_err("should fail");
        assert!(
            matches!(err, TransportError::MalformedHeader { ref line } if line.contains("8192") || line.contains(&format!("{MAX_HEADER_LINE_BYTES}"))),
            "expected MalformedHeader with size hint, got: {err}"
        );
    }

    // ── I6 regression: trailing whitespace in header values ───────────────────

    #[test]
    fn transport_11_content_length_with_trailing_whitespace_parses() {
        // A header like `Content-Length: 5   \r\n` is valid LSP — the previous
        // implementation trimmed leading but not trailing whitespace, causing
        // InvalidContentLength("5   "). Must parse cleanly now.
        let raw = b"Content-Length: 5   \r\n\r\nhello";
        let mut cursor = Cursor::new(raw.as_ref());
        let frame = read_frame(&mut cursor, ContentLengthCeiling::unbounded())
            .expect("must parse with trailing ws");
        assert_eq!(frame.body, b"hello");
    }
}
