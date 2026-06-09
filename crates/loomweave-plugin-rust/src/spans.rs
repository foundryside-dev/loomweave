//! proc-macro2 span → byte/line offsets for entity source ranges.
//!
//! `span-locations` must be enabled (see Cargo.toml). Offsets are relative to
//! the parsed source string, matching what the Python extractor emits as
//! `source.source_byte_start/end` and `source.source_range`.
//!
//! Byte-range spike (resolved empirically, toolchain 1.95.0, proc-macro2 1.x):
//! `proc_macro2::Span::byte_range() -> std::ops::Range<usize>` is available and
//! returns offsets relative to the `syn::parse_file` source string. The
//! unit test `byte_range_is_available_and_correct` proves it compiles and that
//! the reported range slices back to the original token text. No line/column
//! fallback is needed on this toolchain.
use proc_macro2::Span;
use syn::spanned::Spanned;

/// Byte and 1-based line range for any spanned syn node.
#[derive(Debug, Clone, Copy)]
pub struct SourceRange {
    pub byte_start: i64,
    pub byte_end: i64,
    pub start_line: i64,
    pub end_line: i64,
}

pub fn source_range_of(node: &impl Spanned) -> SourceRange {
    range_of_span(node.span())
}

pub fn range_of_span(span: Span) -> SourceRange {
    let bytes = span.byte_range();
    let start = span.start();
    let end = span.end();
    SourceRange {
        byte_start: i64::try_from(bytes.start).unwrap_or(0),
        byte_end: i64::try_from(bytes.end).unwrap_or(0),
        start_line: i64::try_from(start.line).unwrap_or(0),
        end_line: i64::try_from(end.line).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::Item;

    #[test]
    fn byte_range_is_available_and_correct() {
        let src = "pub fn helper(x: i32) -> bool { x > 0 }\n";
        let file = syn::parse_file(src).unwrap();
        let item = &file.items[0];
        let range = source_range_of(item);
        assert!(range.byte_start >= 0);
        assert!(range.byte_end > range.byte_start);
        // The reported byte range slices back to the function text.
        let start = usize::try_from(range.byte_start).unwrap();
        let end = usize::try_from(range.byte_end).unwrap();
        let slice = &src[start..end];
        assert!(slice.starts_with("pub fn helper"));
        assert_eq!(range.start_line, 1);
        // Discriminant check: it really is an Item::Fn we measured.
        assert!(matches!(item, Item::Fn(_)));
    }
}
