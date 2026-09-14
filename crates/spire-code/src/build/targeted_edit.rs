// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Targeted edits: change exactly the node a diagnostic points at.
//!
//! Every autonomous flow in this crate rewrites **whole files**, because that is what the
//! model is asked for and what the structural check verifies. That is safe, but it is not
//! *accountable*: restoring one missing brace comes back as a full-file replacement, so
//! "it fixed the brace" and "it rewrote the function while it was in there" look
//! identical to the person reviewing the result.
//!
//! This module is the primitive for the alternative: locate the smallest AST node that
//! contains a position, and replace **only that span**. Everything outside it is
//! byte-for-byte untouched, so a targeted edit is minimal by construction rather than by
//! promise — and its diff is one hunk, not a file.
//!
//! It is deliberately standalone: no callers yet, no behaviour changed. The flows migrate
//! onto it once it is proven, because the alternative is changing the one path (Fix &
//! Verify) that is currently known to work.

use tree_sitter::Node;

/// A byte range in the source, from the tree-sitter tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeSpan {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl NodeSpan {
    pub fn len(&self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Node kinds that are a sensible unit to rewrite.
///
/// The walk stops at the **first** (innermost) match, so a change to one statement
/// rewrites one statement while a change that needs a whole function — a missing brace, a
/// signature change — rewrites the function. `translation_unit` is deliberately absent:
/// the whole file is not a targeted edit, and a caller that cannot find a unit must fall
/// back to a whole-file rewrite rather than have one chosen silently.
const UNIT_KINDS: &[&str] = &[
    "expression_statement",
    "return_statement",
    "declaration",
    "field_declaration",
    "if_statement",
    "for_statement",
    "while_statement",
    "switch_statement",
    "compound_statement",
    "function_definition",
];

/// The smallest node containing `(line, col)`, widened to a unit.
///
/// `line` and `col` are **0-based**, which is what tree-sitter reports. Compiler
/// diagnostics are 1-based, so callers subtract one; keeping this function in the
/// parser's own convention is what stops the conversion being done twice, in two places,
/// slightly differently.
///
/// `None` means "no unit contains this position" — a comment at file scope, whitespace,
/// past the end. The caller falls back to rewriting the whole file, which is honest: a
/// targeted edit is only better when there is something to target.
pub fn locate_node(source: &str, line: usize, col: usize) -> Option<NodeSpan> {
    let mut parser = crate::build::generic_helpers::cpp_parser_for_spans();
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    // Descend to the deepest node containing the position. Children are ordered, so the
    // first containing child is the only one that can contain it.
    let mut node = root;
    'descend: loop {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if contains(child, line, col) {
                node = child;
                continue 'descend;
            }
        }
        break;
    }

    // Widen to the innermost unit, if there is one before the root.
    let mut candidate = Some(node);
    while let Some(current) = candidate {
        if current.id() == root.id() {
            break;
        }
        if UNIT_KINDS.contains(&current.kind()) {
            return Some(NodeSpan {
                start_byte: current.start_byte(),
                end_byte: current.end_byte(),
            });
        }
        candidate = current.parent();
    }
    None
}

/// Whether `node`'s span contains the 0-based position. The end is exclusive.
fn contains(node: Node<'_>, line: usize, col: usize) -> bool {
    let start = node.start_position();
    let end = node.end_position();
    if line < start.row || line > end.row {
        return false;
    }
    if line == start.row && col < start.column {
        return false;
    }
    if line == end.row && col > end.column {
        return false;
    }
    true
}

/// Replace exactly `span`. Everything outside it survives byte-for-byte.
///
/// Byte slicing is safe here because tree-sitter's offsets always fall on character
/// boundaries of the source it parsed.
pub fn apply_edit(source: &str, span: NodeSpan, replacement: &str) -> String {
    let mut out = String::with_capacity(source.len() + replacement.len());
    out.push_str(&source[..span.start_byte]);
    out.push_str(replacement);
    out.push_str(&source[span.end_byte..]);
    out
}

/// Locate the unit at `(line, col)` and replace it, in one step.
///
/// `None` when there is no unit to target — the caller decides what a fallback means for
/// its own flow (typically a whole-file rewrite).
pub fn targeted_edit(source: &str, line: usize, col: usize, replacement: &str) -> Option<String> {
    let span = locate_node(source, line, col)?;
    Some(apply_edit(source, span, replacement))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A statement change rewrites the statement, and nothing else in the file moves.
    #[test]
    fn rewrites_only_the_statement_at_the_position() {
        let source = "void run() {\n    int a = 1;\n    int b = 2;\n}\n";
        // 0-based: line 2 is `    int b = 2;`, column 8 is inside `b`.
        let span = locate_node(source, 2, 8).expect("a declaration contains that point");
        assert_eq!(&source[span.start_byte..span.end_byte], "int b = 2;");

        let edited = apply_edit(source, span, "int b = 3;");
        assert_eq!(edited, "void run() {\n    int a = 1;\n    int b = 3;\n}\n");
    }

    /// The case that started this: a brace removed mid-function leaves the tree full of
    /// error nodes. Tree-sitter still yields a usable tree, and the point is that a
    /// *targeted* edit is still possible — the located span contains the damage and is
    /// strictly smaller than the file, so the edit is a hunk rather than a rewrite.
    #[test]
    fn finds_a_target_even_when_a_brace_is_missing() {
        let broken = "class Camera {\npublic:\n    void start() \n        int a = 1;\n    \
                      }\n};\n";
        let span = locate_node(broken, 3, 10)
            .expect("the damage should still be inside something locatable");
        assert!(
            span.end_byte - span.start_byte < broken.len(),
            "a targeted edit, not the whole file: {span:?}"
        );
        // The located span has to cover the line the diagnostic would point at.
        let line_start = broken.find("int a = 1;").unwrap();
        assert!(
            span.start_byte <= line_start && span.end_byte >= line_start + "int a = 1;".len(),
            "the span must contain the reported line: {span:?}"
        );
    }

    /// Everything outside the span survives byte-for-byte, including multi-byte
    /// characters — the property that makes "it only changed this" true rather than
    /// merely claimed.
    #[test]
    fn leaves_the_rest_of_the_file_verbatim() {
        let source = "// café — ÿes\nvoid run() {\n    int a = 1;\n}\n";
        let span = locate_node(source, 2, 8).expect("a declaration contains that point");
        let edited = apply_edit(source, span, "int a = 42;");
        assert_eq!(edited, "// café — ÿes\nvoid run() {\n    int a = 42;\n}\n");
    }

    /// No unit contains the point, so there is nothing targeted to do. The caller has to
    /// decide what a fallback means for its own flow; this must not quietly pick the whole
    /// file.
    #[test]
    fn returns_none_when_no_unit_contains_the_point() {
        assert_eq!(locate_node("// just a comment\n", 0, 5), None);
        assert_eq!(locate_node("", 0, 0), None);
    }
}
