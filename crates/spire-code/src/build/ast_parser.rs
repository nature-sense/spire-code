// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Shared tree-sitter AST walker.
//!
//! Converts a tree-sitter CST (for Rust / JavaScript / Python grammars) into
//! the module-agnostic `AstParseResult` consumed by the AST-storage pipeline:
//! significant nodes (functions, classes, imports, variables) are emitted with
//! position/depth/visibility metadata, plus `child` edges (structural nesting)
//! and `calls` edges (resolved by callee name).

use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Language, Node, Parser};

use super::{AstEdgeData, AstNodeData, AstParseResult};

/// Canonical node types produced by the walker.
pub const KIND_FUNCTION: &str = "function";
pub const KIND_CLASS: &str = "class";
pub const KIND_IMPORT: &str = "import";
pub const KIND_VARIABLE: &str = "variable";

/// A tree-sitter grammar together with the mapping from grammar-specific
/// node kinds to canonical AST node types ("function", "class", "import",
/// "variable"). Module build modules that have a real tree-sitter grammar
/// construct one of these and call `parse_with_tree_sitter`.
pub struct LanguageConfig {
    pub language: Language,
    pub lang_name: &'static str,
    pub kind_map: &'static [(&'static str, &'static str)],
}

impl LanguageConfig {
    pub fn new(
        language: Language,
        lang_name: &'static str,
        kind_map: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            language,
            lang_name,
            kind_map,
        }
    }
}

/// Parse `content` using a tree-sitter grammar and convert the CST into an
/// `AstParseResult`. The walker is language-agnostic; all language-specific
/// behaviour (which node kinds are significant, name extraction, modifiers)
/// is driven by `LanguageConfig` + grammar field conventions.
pub fn parse_with_tree_sitter(
    file_path: &Path,
    content: &str,
    content_hash: &str,
    config: &LanguageConfig,
) -> AstParseResult {
    let mut parser = Parser::new();
    if parser.set_language(&config.language).is_err() {
        return AstParseResult {
            file_path: file_path.to_string_lossy().to_string(),
            language: config.lang_name.to_string(),
            content_hash: content_hash.to_string(),
            has_errors: true,
            nodes: Vec::new(),
            edges: Vec::new(),
            docs: Vec::new(),
        };
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => {
            return AstParseResult {
                file_path: file_path.to_string_lossy().to_string(),
                language: config.lang_name.to_string(),
                content_hash: content_hash.to_string(),
                has_errors: true,
                nodes: Vec::new(),
                edges: Vec::new(),
                docs: Vec::new(),
            };
        }
    };

    let kind_map: HashMap<&str, &str> = config.kind_map.iter().cloned().collect();
    let mut walker = AstWalker {
        source: content,
        nodes: Vec::new(),
        edges: Vec::new(),
        kind_map: &kind_map,
        lang_name: config.lang_name,
        function_stack: Vec::new(),
        pending_calls: Vec::new(),
        fn_name_to_index: HashMap::new(),
    };

    walker.walk(tree.root_node(), 0, None, None, 0);
    walker.resolve_calls();

    AstParseResult {
        file_path: file_path.to_string_lossy().to_string(),
        language: config.lang_name.to_string(),
        content_hash: content_hash.to_string(),
        has_errors: tree.root_node().has_error(),
        nodes: walker.nodes,
        edges: walker.edges,
        docs: Vec::new(),
    }
}

/// Pre-configured grammar for Rust source files.
pub fn rust_language_config() -> LanguageConfig {
    LanguageConfig::new(
        tree_sitter_rust::LANGUAGE.into(),
        "Rust",
        &[
            ("function_item", KIND_FUNCTION),
            ("macro_definition", KIND_FUNCTION),
            ("struct_item", KIND_CLASS),
            ("enum_item", KIND_CLASS),
            ("trait_item", KIND_CLASS),
            ("impl_item", KIND_CLASS),
            ("type_item", KIND_CLASS),
            ("use_declaration", KIND_IMPORT),
            ("let_declaration", KIND_VARIABLE),
            ("const_item", KIND_VARIABLE),
            ("static_item", KIND_VARIABLE),
        ],
    )
}

/// Pre-configured grammar for JavaScript / TypeScript source files
/// (tree-sitter-typescript shares the same CST shapes for the kinds we
/// extract, so the JS grammar is a pragmatic parser for both).
pub fn javascript_language_config() -> LanguageConfig {
    LanguageConfig::new(
        tree_sitter_javascript::LANGUAGE.into(),
        "JavaScript",
        &[
            ("function_declaration", KIND_FUNCTION),
            ("generator_function_declaration", KIND_FUNCTION),
            ("arrow_function", KIND_FUNCTION),
            ("method_definition", KIND_FUNCTION),
            ("class_declaration", KIND_CLASS),
            ("import_statement", KIND_IMPORT),
            ("lexical_declaration", KIND_VARIABLE),
            ("variable_declaration", KIND_VARIABLE),
        ],
    )
}

/// Pre-configured grammar for Python source files.
pub fn python_language_config() -> LanguageConfig {
    LanguageConfig::new(
        tree_sitter_python::LANGUAGE.into(),
        "Python",
        &[
            ("function_definition", KIND_FUNCTION),
            ("class_definition", KIND_CLASS),
            ("import_statement", KIND_IMPORT),
            ("import_from_statement", KIND_IMPORT),
            ("assignment", KIND_VARIABLE),
            ("pattern", KIND_VARIABLE),
        ],
    )
}

// ============================================================================
// Walker
// ============================================================================

struct AstWalker<'a> {
    source: &'a str,
    nodes: Vec<AstNodeData>,
    edges: Vec<AstEdgeData>,
    kind_map: &'a HashMap<&'a str, &'a str>,
    lang_name: &'static str,
    /// Stack of significant node indices that are functions (callers).
    function_stack: Vec<usize>,
    /// (caller_idx, callee_name) pairs collected from call expressions.
    pending_calls: Vec<(usize, String)>,
    /// Function name → node index, for resolving call targets.
    fn_name_to_index: HashMap<String, usize>,
}

impl<'a> AstWalker<'a> {
    fn walk(
        &mut self,
        node: Node,
        depth: u32,
        parent_idx: Option<usize>,
        parent_field: Option<&str>,
        sibling_order: u32,
    ) {
        let kind = node.kind();

        if let Some(&canonical) = self.kind_map.get(kind) {
            let idx = self.nodes.len();
            let ast_node = self.make_ast_node(node, canonical, depth);
            self.nodes.push(ast_node);

            if let Some(parent_idx) = parent_idx {
                self.edges.push(AstEdgeData {
                    from_index: parent_idx,
                    to_index: idx,
                    edge_type: "child".to_string(),
                    order: Some(sibling_order),
                    field: parent_field.map(|s| s.to_string()),
                });
            }

            if canonical == KIND_FUNCTION {
                let fn_name = self.nodes[idx].name.clone();
                if let Some(name) = fn_name {
                    self.fn_name_to_index.insert(name, idx);
                    self.function_stack.push(idx);
                }
            }

            self.walk_children(node, depth + 1, Some(idx), parent_field, sibling_order);

            if canonical == KIND_FUNCTION && self.function_stack.last() == Some(&idx) {
                self.function_stack.pop();
            }
        } else {
            // Not a significant node — look for call expressions while
            // passing the current significant parent through unchanged.
            if matches!(kind, "call_expression" | "call" | "method_invocation") {
                self.record_call(node);
            }
            self.walk_children(node, depth, parent_idx, parent_field, sibling_order);
        }
    }

    fn walk_children(
        &mut self,
        node: Node,
        depth: u32,
        parent_idx: Option<usize>,
        parent_field: Option<&str>,
        sibling_order: u32,
    ) {
        let mut cursor = node.walk();
        let mut order = sibling_order;
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                let field = cursor.field_name();
                if child.is_named() {
                    self.walk(child, depth, parent_idx, field, order);
                    order += 1;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        let _ = parent_field;
    }

    /// Build an `AstNodeData` from a significant tree-sitter node.
    fn make_ast_node(&self, node: Node, canonical: &str, depth: u32) -> AstNodeData {
        let start = node.start_position();
        let end = node.end_position();
        let text = self.node_text(node).unwrap_or_default();
        let name = self.extract_name(node, canonical);
        let (is_public, is_async) = self.extract_modifiers(node, canonical, &text, name.as_deref());
        let signature = self.extract_signature(canonical, &text);
        let return_type = self.extract_return_type(node);

        AstNodeData {
            node_type: canonical.to_string(),
            name,
            text,
            start_line: (start.row as u32) + 1,
            start_col: start.column as u32,
            end_line: (end.row as u32) + 1,
            end_col: end.column as u32,
            depth,
            is_public,
            is_async,
            signature,
            return_type,
            children: Vec::new(),
        }
    }

    fn extract_name(&self, node: Node, canonical: &str) -> Option<String> {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.node_text(name_node).unwrap_or_default();
            if !name.is_empty() {
                return Some(name);
            }
        }
        match canonical {
            KIND_IMPORT => {
                let t = self.node_text(node).unwrap_or_default();
                let cleaned = t
                    .trim()
                    .trim_start_matches("import ")
                    .trim_start_matches("from ")
                    .trim_start_matches("use ")
                    .replace([';', '\'', '"'], "")
                    .trim()
                    .to_string();
                if cleaned.is_empty() {
                    None
                } else {
                    Some(cleaned)
                }
            }
            KIND_VARIABLE => self.find_first_identifier(node, 2),
            _ => None,
        }
    }

    /// Recursively look for the first `name` field under a node (used for
    /// variable declarations whose name lives in a nested `variable_declarator`).
    fn find_first_identifier(&self, node: Node, max_depth: u32) -> Option<String> {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = self.node_text(name_node).unwrap_or_default();
            if !name.is_empty() {
                return Some(name);
            }
        }
        if max_depth == 0 {
            return None;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(name) = self.find_first_identifier(child, max_depth - 1) {
                return Some(name);
            }
        }
        None
    }

    fn extract_modifiers(
        &self,
        node: Node,
        canonical: &str,
        text: &str,
        name: Option<&str>,
    ) -> (bool, bool) {
        let trimmed = text.trim_start();
        let is_public = match canonical {
            KIND_IMPORT | KIND_VARIABLE => false,
            _ => match self.lang_name {
                "Rust" => {
                    trimmed.starts_with("pub")
                        || node.child_by_field_name("visibility_modifier").is_some()
                }
                "JavaScript" => trimmed.starts_with("export"),
                "Python" => name.is_some_and(|n| !n.starts_with('_')),
                _ => false,
            },
        };
        let is_async = text.contains("async")
            || text.trim_start().starts_with("async ")
            || text.trim_start().starts_with("async def ");
        (is_public, is_async)
    }

    fn extract_signature(&self, canonical: &str, text: &str) -> Option<String> {
        if canonical == KIND_FUNCTION {
            let trimmed = text.trim();
            let cut = trimmed.find(['{', ';', ':']).unwrap_or(trimmed.len());
            Some(trimmed[..cut].trim().to_string())
        } else {
            Some(text.trim().to_string())
        }
    }

    fn extract_return_type(&self, node: Node) -> Option<String> {
        let rt = node.child_by_field_name("return_type")?;
        let t = self.node_text(rt)?;
        let cleaned = t
            .trim()
            .trim_start_matches("-> ")
            .trim_start_matches("->")
            .trim();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned.to_string())
        }
    }

    /// Record a `(caller_idx, callee_name)` for call expressions inside the
    /// current function.
    fn record_call(&mut self, node: Node) {
        let callee = self.extract_callee_name(node);
        if let (Some(caller_idx), Some(name)) = (self.function_stack.last().copied(), callee) {
            self.pending_calls.push((caller_idx, name));
        }
    }

    fn extract_callee_name(&self, node: Node) -> Option<String> {
        if let Some(f) = node.child_by_field_name("function") {
            if let Some(t) = self.node_text(f) {
                let name = t
                    .split(['(', ' ', '.', '<'])
                    .next()
                    .unwrap_or(&t)
                    .trim()
                    .to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let t = self.node_text(child)?;
            let first = t.chars().next()?;
            if !t.contains(' ') && (first.is_alphabetic() || first == '_') {
                let name = t
                    .split(['(', ' ', '.', '<'])
                    .next()
                    .unwrap_or(&t)
                    .trim()
                    .to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        None
    }

    /// After the walk completes, resolve pending call pairs into `calls` edges
    /// to functions we actually captured.
    fn resolve_calls(&mut self) {
        let pending = std::mem::take(&mut self.pending_calls);
        for (caller_idx, callee_name) in pending {
            if let Some(&callee_idx) = self.fn_name_to_index.get(&callee_name) {
                if caller_idx != callee_idx {
                    self.edges.push(AstEdgeData {
                        from_index: caller_idx,
                        to_index: callee_idx,
                        edge_type: "calls".to_string(),
                        order: None,
                        field: None,
                    });
                }
            }
        }
    }

    fn node_text(&self, node: Node) -> Option<String> {
        node.utf8_text(self.source.as_bytes())
            .ok()
            .map(|s| s.to_string())
    }
}