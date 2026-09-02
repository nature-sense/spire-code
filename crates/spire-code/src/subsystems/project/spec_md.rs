// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Spec Markdown** — the human-friendly, deterministic projection of an
//! [`AppSpec`] (equivalently of its graph-native form in [`spec_graph`]).
//!
//! Two directions, both pure:
//! - [`spec_to_markdown`] renders an [`AppSpec`] to a strict Markdown document;
//! - [`markdown_to_spec`] parses that same document back into an [`AppSpec`].
//!
//! Round-trip is a hard property (`markdown_to_spec(spec_to_markdown(s)) == s`),
//! so the Markdown and the graph (via [`spec_graph::decompose`]/[`reconstruct`])
//! are the same spec in two views — 1:1 by construction, never a promise to
//! keep in sync.

use super::spec::{
    ActorSpec, AppMeta, AppSpec, DomainType, Field, GraphEdgeType, GraphNodeType, GraphSchema,
    LayoutNode, Screen, Type, UiAction, UiBinding, UiNavigation,
};

// ── Markdown type expressions ─────────────────────────────────────────────
// Friendly but unambiguous: primitives plain; named types bare identifiers;
// `X?` for option<X>; `list<X>`; inline `record(f:T;…)`.

fn md_type(ty: &Type) -> String {
    match ty {
        Type::Str => "str".to_string(),
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Named { name } => name.clone(),
        Type::Option { of } => format!("{}?", md_type(of)),
        Type::List { of } => format!("list<{}>", md_type(of)),
        Type::Record { fields } => {
            let inner = fields
                .iter()
                .map(|f| format!("{}:{}", f.name, md_type(&f.ty)))
                .collect::<Vec<_>>()
                .join(";");
            format!("record({inner})")
        }
    }
}

struct MdTypeParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> MdTypeParser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().peekable(),
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.peek().copied()
    }

    fn eat(&mut self, c: char) -> Result<(), String> {
        match self.chars.next() {
            Some(x) if x == c => Ok(()),
            other => Err(format!("expected '{c}', got {other:?}")),
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        self.skip_ws();
        let mut out = String::new();
        while let Some(c) = self.chars.peek() {
            if c.is_alphanumeric() || *c == '_' || *c == '/' || *c == '.' || *c == '-' {
                out.push(*c);
                self.chars.next();
            } else {
                break;
            }
        }
        if out.is_empty() {
            Err("expected an identifier".to_string())
        } else {
            Ok(out)
        }
    }

    fn parse(&mut self) -> Result<Type, String> {
        let name = self.ident()?;
        let ty = match name.as_str() {
            "str" => Type::Str,
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "list" => {
                self.eat('<')?;
                let of = Box::new(self.parse()?);
                self.eat('>')?;
                Type::List { of }
            }
            "record" => {
                self.eat('(')?;
                let mut fields = Vec::new();
                if self.peek() == Some(')') {
                    self.chars.next();
                } else {
                    loop {
                        let fname = self.ident()?;
                        self.eat(':')?;
                        let fty = self.parse()?;
                        fields.push(Field {
                            name: fname,
                            ty: fty,
                        });
                        match self.peek() {
                            Some(';') => {
                                self.chars.next();
                            }
                            Some(')') => {
                                self.chars.next();
                                break;
                            }
                            other => return Err(format!("expected ';' or ')', got {other:?}")),
                        }
                    }
                }
                Type::Record { fields }
            }
            other => Type::Named {
                name: other.to_string(),
            },
        };
        // Optional suffix is valid after *any* parsed type, at any nesting.
        if self.peek() == Some('?') {
            self.chars.next();
            Ok(Type::Option { of: Box::new(ty) })
        } else {
            Ok(ty)
        }
    }
}

fn parse_md_type(s: &str) -> Result<Type, String> {
    let mut p = MdTypeParser::new(s);
    let ty = p.parse()?;
    if p.peek().is_some() {
        return Err(format!("trailing chars in type '{s}'"));
    }
    Ok(ty)
}

// ── Layout sketch grammar ─────────────────────────────────────────────────
// `vstack(...)`, `hstack(...)`, `list(item)`, `text("…")`, `spacer`, `empty`,
// `button("label")->action`, `input("placeholder")@bind`. Children are
// comma-separated; quotes may be escaped with \".

fn md_layout(layout: &LayoutNode) -> String {
    match layout {
        LayoutNode::Empty => "empty".to_string(),
        LayoutNode::VStack { children } => {
            format!(
                "vstack({})",
                children
                    .iter()
                    .map(md_layout)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        LayoutNode::HStack { children } => {
            format!(
                "hstack({})",
                children
                    .iter()
                    .map(md_layout)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        LayoutNode::List { item } => format!("list({})", md_layout(item)),
        LayoutNode::Text { text } => format!("text(\"{}\")", escape_quotes(text)),
        LayoutNode::Button { label, action } => {
            format!("button(\"{}\")->{action}", escape_quotes(label))
        }
        LayoutNode::Input { placeholder, bind } => {
            format!("input(\"{}\")@{bind}", escape_quotes(placeholder))
        }
        LayoutNode::Spacer => "spacer".to_string(),
    }
}

fn escape_quotes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn unescape_quotes(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else {
            out.push(c);
        }
    }
    out
}

struct LayoutParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> LayoutParser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().peekable(),
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.peek().copied()
    }

    fn next(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.next()
    }

    fn quoted(&mut self) -> Result<String, String> {
        if self.next() != Some('"') {
            return Err("expected opening quote".to_string());
        }
        let mut out = String::new();
        let mut esc = false;
        while let Some(c) = self.chars.next() {
            if esc {
                out.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                return Ok(out);
            } else {
                out.push(c);
            }
        }
        Err("unterminated quote".to_string())
    }

    fn suffix(&mut self) -> Result<Option<String>, String> {
        if self.next() == Some('-') {
            if self.next() != Some('>') {
                return Err("expected '>'".to_string());
            }
            Ok(Some(self.identifier()?))
        } else {
            Ok(None)
        }
    }

    fn identifier(&mut self) -> Result<String, String> {
        self.skip_ws();
        let mut out = String::new();
        while let Some(c) = self.chars.peek() {
            if c.is_alphanumeric() || *c == '_' || *c == '/' || *c == '.' || *c == '-' {
                out.push(*c);
                self.chars.next();
            } else {
                break;
            }
        }
        if out.is_empty() {
            Err("expected an identifier".to_string())
        } else {
            Ok(out)
        }
    }

    fn parse_node(&mut self) -> Result<LayoutNode, String> {
        self.skip_ws();
        let name = self.identifier()?;
        Ok(match name.as_str() {
            "empty" => LayoutNode::Empty,
            "spacer" => LayoutNode::Spacer,
            "vstack" => {
                if self.next() != Some('(') {
                    return Err("expected '('".to_string());
                }
                let children = self.children_until(')')?;
                LayoutNode::VStack { children }
            }
            "hstack" => {
                if self.next() != Some('(') {
                    return Err("expected '('".to_string());
                }
                let children = self.children_until(')')?;
                LayoutNode::HStack { children }
            }
            "list" => {
                if self.next() != Some('(') {
                    return Err("expected '('".to_string());
                }
                let item = Box::new(self.parse_node()?);
                if self.next() != Some(')') {
                    return Err("expected ')'".to_string());
                }
                LayoutNode::List { item }
            }
            "text" => {
                if self.next() != Some('(') {
                    return Err("expected '('".to_string());
                }
                let text = self.quoted()?;
                if self.next() != Some(')') {
                    return Err("expected ')'".to_string());
                }
                LayoutNode::Text { text }
            }
            "button" => {
                if self.next() != Some('(') {
                    return Err("expected '('".to_string());
                }
                let label = self.quoted()?;
                if self.next() != Some(')') {
                    return Err("expected ')'".to_string());
                }
                let action = self
                    .suffix()?
                    .ok_or_else(|| "button missing '->action'".to_string())?;
                LayoutNode::Button { label, action }
            }
            "input" => {
                if self.next() != Some('(') {
                    return Err("expected '('".to_string());
                }
                let placeholder = self.quoted()?;
                if self.next() != Some(')') {
                    return Err("expected ')'".to_string());
                }
                let bind = if self.next() == Some('@') {
                    self.identifier()?
                } else {
                    String::new()
                };
                LayoutNode::Input { placeholder, bind }
            }
            other => return Err(format!("unknown layout kind '{other}'")),
        })
    }

    fn children_until(&mut self, terminator: char) -> Result<Vec<LayoutNode>, String> {
        let mut out = Vec::new();
        if self.peek() == Some(terminator) {
            self.next();
            return Ok(out);
        }
        loop {
            out.push(self.parse_node()?);
            match self.next() {
                Some(',') => {}
                Some(c) if c == terminator => break,
                Some(c) => return Err(format!("expected ',' or '{terminator}', got '{c}'")),
                None => return Err("unterminated container".to_string()),
            }
        }
        Ok(out)
    }
}

fn parse_layout(s: &str) -> Result<LayoutNode, String> {
    let mut p = LayoutParser::new(s);
    let node = p.parse_node()?;
    if p.peek().is_some() {
        return Err("trailing layout content".to_string());
    }
    Ok(node)
}

// ── Render: AppSpec → Markdown ───────────────────────────────────────────

fn md_fields(fields: &[Field]) -> Vec<String> {
    fields
        .iter()
        .map(|f| format!("{}: {}", f.name, md_type(&f.ty)))
        .collect()
}

fn md_joined(items: &[String]) -> String {
    items.join(", ")
}

/// Render an [`AppSpec`] to the strict, human-friendly spec Markdown.
pub fn spec_to_markdown(spec: &AppSpec) -> String {
    let mut out = String::new();
    out.push_str(&format!("# AppSpec: {}\n\n", spec.app.name));
    out.push_str(&format!("**Goal**: {}\n", spec.app.goal));

    // ── data types ────────────────────────────────────────────────────────
    if !spec.types.is_empty() {
        out.push_str("\n## Data types\n");
        for t in &spec.types {
            match t {
                DomainType::Record { name, fields } => {
                    out.push_str(&format!("\n### `{name}` (record)\n"));
                    out.push_str("| field | type |\n|-------|------|\n");
                    for f in fields {
                        out.push_str(&format!("| {} | {} |\n", f.name, md_type(&f.ty)));
                    }
                }
                DomainType::Enum { name, variants } => {
                    out.push_str(&format!("\n### `{name}` (enum)\n"));
                    out.push_str(&format!("{}\n", variants.join(" | ")));
                }
            }
        }
    }

    // ── graph schema ──────────────────────────────────────────────────────
    if !spec.graph.nodes.is_empty() || !spec.graph.edges.is_empty() {
        out.push_str("\n## Graph\n");
        if !spec.graph.nodes.is_empty() {
            out.push_str("\n### nodes\n");
            for n in &spec.graph.nodes {
                if n.description.trim().is_empty() {
                    out.push_str(&format!("- **{}**\n", n.name));
                } else {
                    out.push_str(&format!("- **{}** — {}\n", n.name, n.description));
                }
                for f in &n.fields {
                    out.push_str(&format!("  - {}: {}\n", f.name, md_type(&f.ty)));
                }
            }
        }
        if !spec.graph.edges.is_empty() {
            out.push_str("\n### edges\n");
            out.push_str(
                "| name | from | to | description |\n|------|------|----|-------------|\n",
            );
            for e in &spec.graph.edges {
                out.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    e.name, e.from, e.to, e.description
                ));
            }
        }
    }

    // ── backend actors ────────────────────────────────────────────────────
    if !spec.actors.is_empty() {
        out.push_str("\n## Backend\n");
        for a in &spec.actors {
            if a.description.trim().is_empty() {
                out.push_str(&format!("\n### `{}`\n", a.name));
            } else {
                out.push_str(&format!("\n### `{}` — {}\n", a.name, a.description));
            }
            if !a.handlers.is_empty() {
                out.push_str(&format!("Handlers: {}\n", a.handlers.join(", ")));
            }
            if !a.state.is_empty() {
                out.push_str(&format!("State: {}\n", md_joined(&md_fields(&a.state))));
            }
            if !a.uses.is_empty() {
                out.push_str(&format!("Uses: {}\n", a.uses.join(", ")));
            }
        }
    }

    // ── UI screens ────────────────────────────────────────────────────────
    if !spec.ui.is_empty() {
        out.push_str("\n## UI\n");
        for s in &spec.ui {
            if s.title.trim().is_empty() {
                out.push_str(&format!("\n### {}\n", s.id));
            } else {
                out.push_str(&format!("\n### {} — {}\n", s.id, s.title));
            }
            out.push_str(&format!("Layout: {}\n", md_layout(&s.layout)));
            let actions = s
                .actions
                .iter()
                .map(|a| {
                    if a.description.trim().is_empty() {
                        format!("{}->{}", a.id, a.bridge)
                    } else {
                        format!(
                            "{}(\"{}\")->{}",
                            a.id,
                            escape_quotes(&a.description),
                            a.bridge
                        )
                    }
                })
                .collect::<Vec<_>>();
            if !actions.is_empty() {
                out.push_str(&format!("Actions: {}\n", actions.join(", ")));
            }
            let bindings = s
                .bindings
                .iter()
                .map(|b| format!("{}<-{}", b.field, b.method))
                .collect::<Vec<_>>();
            if !bindings.is_empty() {
                out.push_str(&format!("Bindings: {}\n", bindings.join(", ")));
            }
            let navigation = s
                .navigation
                .iter()
                .map(|n| format!("{}->{}", n.action_id, n.to))
                .collect::<Vec<_>>();
            if !navigation.is_empty() {
                out.push_str(&format!("Navigation: {}\n", navigation.join(", ")));
            }
        }
    }

    out.push('\n');
    out
}

// ── Parse: Markdown → AppSpec ────────────────────────────────────────────

struct MdLineIter<'a> {
    lines: std::iter::Peekable<std::str::Lines<'a>>,
}

impl<'a> MdLineIter<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            lines: src.lines().peekable(),
        }
    }

    fn peek(&mut self) -> Option<&'a str> {
        self.lines.peek().copied()
    }

    fn next_trimmed(&mut self) -> Option<String> {
        self.lines.next().map(|l| l.trim().to_string())
    }

    /// Advance to the next non-blank, non-table-separator line.
    fn next_meaningful(&mut self) -> Option<String> {
        while let Some(l) = self.next_trimmed() {
            if l.is_empty() || l.starts_with("|---") || l == "---" {
                continue;
            }
            return Some(l);
        }
        None
    }

    fn expect_meaningful(&mut self, what: &str) -> Result<String, String> {
        self.next_meaningful()
            .ok_or_else(|| format!("unexpected end of Markdown while reading {what}"))
    }

    /// Consume the next raw line (caller already peeked it).
    fn consume(&mut self) {
        self.lines.next();
    }
}

fn strip_heading(l: &str, prefix: &str) -> Option<String> {
    l.strip_prefix(prefix).map(|s| s.trim().to_string())
}

/// `### `Name` (record)` | `### `Name` (enum)`
fn parse_type_heading(l: &str) -> Option<(String, String)> {
    let rest = strip_heading(l, "### `")?;
    let (name, kind) = rest.rsplit_once("` (")?;
    if !kind.ends_with(')') {
        return None;
    }
    Some((name.to_string(), kind[..kind.len() - 1].to_string()))
}

/// `### `Actor` — description` (description optional)
fn parse_actor_heading(l: &str) -> Option<(String, String)> {
    let rest = strip_heading(l, "### `")?;
    let (name, desc) = match rest.rsplit_once("` — ") {
        Some((n, d)) => (n, d),
        None if rest.ends_with('`') => (&rest[..rest.len() - 1], ""),
        None => return None,
    };
    Some((name.to_string(), desc.trim().to_string()))
}

/// `### id — title` (title optional)
fn parse_screen_heading(l: &str) -> Option<(String, String)> {
    let rest = strip_heading(l, "### ")?;
    if rest.contains('`') {
        return None;
    }
    let (id, title) = match rest.rsplit_once(" — ") {
        Some((i, t)) => (i.trim(), t.trim()),
        None => (rest.trim(), ""),
    };
    Some((id.to_string(), title.to_string()))
}

fn split_list(body: &str, label: &str) -> Result<Vec<String>, String> {
    let body = strip_heading(body, &format!("{label}: "))
        .ok_or_else(|| format!("expected '{label}: ' line"))?;
    if body.is_empty() {
        return Ok(Vec::new());
    }
    Ok(body.split(',').map(|s| s.trim().to_string()).collect())
}

/// Split on commas that sit *outside* double quotes (action descriptions are
/// quoted and may contain commas). Handles the same backslash escapes as
/// quoted strings elsewhere.
fn split_quoted_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut esc = false;
    for c in s.chars() {
        if in_quote {
            cur.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_quote = false;
            }
        } else if c == '"' {
            in_quote = true;
            cur.push(c);
        } else if c == ',' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parse the `Actions:` body: `id->method` or `id("description")->method`,
/// comma-separated at quote depth zero.
fn parse_actions(body: &str) -> Result<Vec<UiAction>, String> {
    let mut out = Vec::new();
    for item in split_quoted_commas(body) {
        let (lhs, bridge) = item
            .split_once("->")
            .ok_or_else(|| format!("action '{item}' lacks '->bridge'"))?;
        let (id, description) = match lhs.find('(') {
            None => (lhs.trim().to_string(), String::new()),
            Some(p) => {
                let (id_part, rest) = lhs.split_at(p);
                let rest = rest.trim();
                let inner = rest
                    .strip_prefix('(')
                    .and_then(|r| r.strip_suffix(')'))
                    .ok_or_else(|| format!("action '{item}': malformed description"))?;
                let quoted = inner
                    .trim()
                    .strip_prefix('"')
                    .and_then(|q| q.strip_suffix('"'))
                    .ok_or_else(|| format!("action '{item}': description must be quoted"))?;
                (id_part.trim().to_string(), unescape_quotes(quoted))
            }
        };
        out.push(UiAction {
            id,
            description,
            bridge: bridge.trim().to_string(),
        });
    }
    Ok(out)
}

/// Parse an AppSpec body produced by [`spec_to_markdown`]. Returns the
/// reconstructed spec or a descriptive error.
pub fn markdown_to_spec(md: &str) -> Result<AppSpec, String> {
    let mut it = MdLineIter::new(md);

    // Header.
    let header = it.expect_meaningful("the '# AppSpec:' header")?;
    let name = strip_heading(&header, "# AppSpec: ")
        .ok_or_else(|| format!("expected '# AppSpec: <name>', got '{header}'"))?;
    let goal_line = it.expect_meaningful("the '**Goal**:' line")?;
    let goal = strip_heading(&goal_line, "**Goal**: ")
        .ok_or_else(|| format!("expected '**Goal**: …', got '{goal_line}'"))?;

    let mut spec = AppSpec {
        app: AppMeta { name, goal },
        types: Vec::new(),
        graph: GraphSchema::default(),
        actors: Vec::new(),
        bridge: Vec::new(),
        ui: Vec::new(),
    };

    #[derive(PartialEq)]
    enum Section {
        Header,
        Types,
        Graph,
        GraphNodes,
        GraphEdges,
        Backend,
        Ui,
    }

    // Line-type dispatch: which section header does a line match (if any)?
    let section_of = |l: &str| -> Option<Section> {
        if l.starts_with("## Data types") {
            Some(Section::Types)
        } else if l == "## Graph" {
            Some(Section::Graph)
        } else if l == "### nodes" {
            Some(Section::GraphNodes)
        } else if l == "### edges" {
            Some(Section::GraphEdges)
        } else if l.starts_with("## Backend") {
            Some(Section::Backend)
        } else if l.starts_with("## UI") {
            Some(Section::Ui)
        } else {
            None
        }
    };

    let mut current = Section::Header;

    while let Some(raw) = it.next_meaningful() {
        // Explicit section boundaries.
        if let Some(sec) = section_of(&raw) {
            current = sec;
            continue;
        }
        if current == Section::Header {
            // Body of the AppSpec has no free-form text between goal and sections.
            continue;
        }

        if current == Section::Types {
            if let Some((tname, kind)) = parse_type_heading(&raw) {
                match kind.as_str() {
                    "record" => {
                        // `| field | type |` header row, then `| f | ty |` rows.
                        // Peek-based so we never consume the next heading/section.
                        if it.peek().is_some_and(|l| l.trim() == "| field | type |") {
                            it.consume();
                        }
                        let mut fields = Vec::new();
                        loop {
                            let Some(row) = it.peek() else {
                                break;
                            };
                            let row = row.trim();
                            if row.is_empty() {
                                it.consume();
                                continue;
                            }
                            if row.starts_with("|---") || row == "| field | type |" {
                                it.consume();
                                continue;
                            }
                            if !row.starts_with('|') {
                                break;
                            }
                            it.consume();
                            let cols = row.trim_matches('|');
                            let (fname, fty) = cols
                                .split_once('|')
                                .ok_or_else(|| format!("bad field row {cols}"))?;
                            fields.push(Field {
                                name: fname.trim().to_string(),
                                ty: parse_md_type(fty.trim())?,
                            });
                        }
                        spec.types.push(DomainType::Record {
                            name: tname,
                            fields,
                        });
                    }
                    "enum" => {
                        let variants_line =
                            it.expect_meaningful(&format!("variants of '{tname}'"))?;
                        let variants = variants_line
                            .split('|')
                            .map(|v| v.trim().to_string())
                            .collect();
                        spec.types.push(DomainType::Enum {
                            name: tname,
                            variants,
                        });
                    }
                    other => return Err(format!("unknown type kind '{other}' for '{tname}'"))?,
                }
            } else if raw.starts_with("### `") {
                return Err(format!("unparsable type heading: '{raw}'"));
            }
            continue;
        }

        if current == Section::Graph || current == Section::GraphNodes {
            if let Some(name) = strip_heading(&raw, "- **") {
                let (nname, desc) = match name.rsplit_once("** — ") {
                    Some((n, d)) => (n.to_string(), d.to_string()),
                    None => {
                        let n = name.trim_end_matches("**").to_string();
                        (n, String::new())
                    }
                };
                let mut fields = Vec::new();
                while let Some(l) = it.peek() {
                    // Field bullets keep their two-space indent in the raw line.
                    let Some(body) = l.strip_prefix("  - ") else {
                        break;
                    };
                    it.consume();
                    let body = body.trim();
                    let (fname, fty) = body
                        .split_once(':')
                        .ok_or_else(|| format!("bad graph-node field {body}"))?;
                    fields.push(Field {
                        name: fname.trim().to_string(),
                        ty: parse_md_type(fty.trim())?,
                    });
                }
                spec.graph.nodes.push(GraphNodeType {
                    name: nname,
                    description: desc,
                    fields,
                });
            } else if raw.starts_with("- ") {
                return Err(format!("bad graph node line '{raw}'"));
            }
            continue;
        }

        if current == Section::GraphEdges {
            if raw.starts_with('|') {
                if raw == "| name | from | to | description |" {
                    continue;
                }
                let cols: Vec<String> = raw
                    .trim_matches('|')
                    .split('|')
                    .map(|c| c.trim().to_string())
                    .collect();
                if let [name, from, to, description] = cols.as_slice() {
                    if !name.is_empty() {
                        spec.graph.edges.push(GraphEdgeType {
                            name: name.clone(),
                            from: from.clone(),
                            to: to.clone(),
                            description: description.clone(),
                        });
                    }
                }
            }
            continue;
        }

        if current == Section::Backend {
            if let Some((aname, adesc)) = parse_actor_heading(&raw) {
                spec.actors.push(ActorSpec {
                    name: aname,
                    description: adesc,
                    handlers: Vec::new(),
                    state: Vec::new(),
                    uses: Vec::new(),
                });
            } else if let Some(last) = spec.actors.last_mut() {
                if raw.starts_with("Handlers: ") {
                    last.handlers = split_list(&raw, "Handlers")?;
                } else if raw.starts_with("State: ") {
                    if let Some(state) = strip_heading(&raw, "State: ") {
                        if !state.is_empty() {
                            for f in state.split(',') {
                                let f = f.trim();
                                let (fnm, fty) = f
                                    .split_once(':')
                                    .ok_or_else(|| format!("bad state field '{f}'"))?;
                                last.state.push(Field {
                                    name: fnm.trim().to_string(),
                                    ty: parse_md_type(fty.trim())?,
                                });
                            }
                        }
                    }
                } else if raw.starts_with("Uses: ") {
                    last.uses = split_list(&raw, "Uses")?;
                } else {
                    return Err(format!("unexpected backend line '{raw}'"));
                }
            } else {
                return Err("actor body before any actor heading".to_string());
            }
            continue;
        }

        if current == Section::Ui {
            if let Some((sid, stitle)) = parse_screen_heading(&raw) {
                let layout_line = it.expect_meaningful(&format!("Layout for '{sid}'"))?;
                let layout = strip_heading(&layout_line, "Layout: ")
                    .ok_or_else(|| format!("expected 'Layout: …', got '{layout_line}'"))?;
                let layout = parse_layout(&layout)?;
                let mut actions = Vec::new();
                let mut bindings = Vec::new();
                let mut navigation = Vec::new();
                while let Some(l) = it.peek() {
                    let l = l.trim();
                    if l.starts_with("Actions: ") {
                        it.consume();
                        let body = strip_heading(l, "Actions: ")
                            .ok_or_else(|| "expected 'Actions: ' line".to_string())?;
                        actions = parse_actions(&body)?;
                    } else if l.starts_with("Bindings: ") {
                        it.consume();
                        bindings = split_list(l, "Bindings")?
                            .into_iter()
                            .filter_map(|b| {
                                b.split_once("<-").map(|(field, method)| UiBinding {
                                    field: field.trim().to_string(),
                                    method: method.trim().to_string(),
                                })
                            })
                            .collect();
                    } else if l.starts_with("Navigation: ") {
                        it.consume();
                        navigation = split_list(l, "Navigation")?
                            .into_iter()
                            .filter_map(|n| {
                                n.split_once("->").map(|(action_id, to)| UiNavigation {
                                    action_id: action_id.trim().to_string(),
                                    to: to.trim().to_string(),
                                })
                            })
                            .collect();
                    } else {
                        break;
                    }
                }
                spec.ui.push(Screen {
                    id: sid,
                    title: stitle,
                    layout,
                    actions,
                    bindings,
                    navigation,
                });
            } else if raw.starts_with("### ") {
                return Err(format!("unparsable screen heading: '{raw}'"));
            } else {
                return Err(format!("unexpected UI line '{raw}'"));
            }
            continue;
        }
    }

    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystems::project::spec::UiAction;

    fn assert_eq_modulo_bridge(a: &AppSpec, b: &AppSpec) {
        assert_eq!(a.app, b.app);
        assert_eq!(a.types, b.types, "types mismatch");
        assert_eq!(a.graph, b.graph, "graph mismatch");
        assert_eq!(a.actors, b.actors, "actors mismatch");
        assert_eq!(a.ui, b.ui, "ui mismatch");
    }

    /// A compact but non-trivial spec exercising every Markdown construct.
    fn kitchen_sink_spec() -> AppSpec {
        use crate::subsystems::project::spec::{
            ActorSpec, AppMeta, BridgeMethod, DomainType, Field, GraphEdgeType, GraphNodeType,
            GraphSchema, LayoutNode, Screen, Type, UiBinding, UiNavigation,
        };
        AppSpec {
            app: AppMeta {
                name: "kitchen-sink".into(),
                goal: "Exercise every spec_md construct".into(),
            },
            types: vec![
                DomainType::Record {
                    name: "Feature".into(),
                    fields: vec![
                        Field {
                            name: "id".into(),
                            ty: Type::Str,
                        },
                        Field {
                            name: "rank".into(),
                            ty: Type::Int,
                        },
                        Field {
                            name: "maybe".into(),
                            ty: Type::Option {
                                of: Box::new(Type::Named { name: "Geo".into() }),
                            },
                        },
                        Field {
                            name: "tags".into(),
                            ty: Type::List {
                                of: Box::new(Type::Str),
                            },
                        },
                        Field {
                            name: "meta".into(),
                            ty: Type::Record {
                                fields: vec![
                                    Field {
                                        name: "owner".into(),
                                        ty: Type::Str,
                                    },
                                    Field {
                                        name: "ok".into(),
                                        ty: Type::Bool,
                                    },
                                ],
                            },
                        },
                    ],
                },
                DomainType::Record {
                    name: "Empty".into(),
                    fields: vec![],
                },
                DomainType::Enum {
                    name: "Geo".into(),
                    variants: vec!["point".into(), "line".into(), "polygon".into()],
                },
            ],
            graph: GraphSchema {
                nodes: vec![GraphNodeType {
                    name: "flood_zone".into(),
                    description: "areas at flood risk".into(),
                    fields: vec![
                        Field {
                            name: "id".into(),
                            ty: Type::Str,
                        },
                        Field {
                            name: "zone".into(),
                            ty: Type::Named { name: "Geo".into() },
                        },
                    ],
                }],
                edges: vec![GraphEdgeType {
                    name: "CONTAINS".into(),
                    from: "flood_zone".into(),
                    to: "building".into(),
                    description: "links a zone to what sits on it".into(),
                }],
            },
            actors: vec![ActorSpec {
                name: "MapActor".into(),
                description: "owns the map".into(),
                handlers: vec!["map/list".into(), "map/zoom".into()],
                state: vec![
                    Field {
                        name: "layers".into(),
                        ty: Type::List {
                            of: Box::new(Type::Named {
                                name: "Feature".into(),
                            }),
                        },
                    },
                    Field {
                        name: "zoom".into(),
                        ty: Type::Option {
                            of: Box::new(Type::Int),
                        },
                    },
                ],
                uses: vec!["build_types".into(), "config".into()],
            }],
            bridge: vec![BridgeMethod {
                method: "map/list".into(),
                description: "list matching layers".into(),
                params: vec![Field {
                    name: "bbox".into(),
                    ty: Type::Str,
                }],
                result: Type::Named {
                    name: "Feature".into(),
                },
            }],
            ui: vec![Screen {
                id: "map".into(),
                title: "Map view".into(),
                layout: LayoutNode::VStack {
                    children: vec![
                        LayoutNode::HStack {
                            children: vec![
                                LayoutNode::Button {
                                    label: "Reload \"now\"".into(),
                                    action: "reload".into(),
                                },
                                LayoutNode::Input {
                                    placeholder: "layer name".into(),
                                    bind: "layerName".into(),
                                },
                                LayoutNode::Text {
                                    text: "a \\ b".into(),
                                },
                                LayoutNode::Spacer,
                            ],
                        },
                        LayoutNode::List {
                            item: Box::new(LayoutNode::Button {
                                label: "Open".into(),
                                action: "select".into(),
                            }),
                        },
                        LayoutNode::Empty,
                    ],
                },
                actions: vec![UiAction {
                    id: "reload".into(),
                    description: "Reload the layer list".into(),
                    bridge: "map/list".into(),
                }],
                bindings: vec![UiBinding {
                    field: "layers".into(),
                    method: "map/list".into(),
                }],
                navigation: vec![UiNavigation {
                    action_id: "select".into(),
                    to: "inspector".into(),
                }],
            }],
        }
    }

    #[test]
    fn md_roundtrips_kitchen_sink_spec() {
        let spec = kitchen_sink_spec();
        let md = spec_to_markdown(&spec);
        let back = markdown_to_spec(&md).expect("markdown parses");
        assert_eq_modulo_bridge(&back, &spec);
    }

    #[test]
    fn md_full_reference_spec_roundtrips() {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        let spec: AppSpec = serde_json::from_str(raw).expect("reference spec parses");
        assert!(spec.is_valid());
        let md = spec_to_markdown(&spec);
        let back = markdown_to_spec(&md).expect("reference markdown parses");
        assert_eq!(back.app, spec.app);
        assert_eq!(back.types, spec.types, "types mismatch");
        assert_eq!(back.graph, spec.graph, "graph mismatch");
        assert_eq!(back.actors, spec.actors, "actors mismatch");
        assert_eq!(back.ui, spec.ui, "ui mismatch");
    }

    /// Rendering is a deterministic function: render(parse(render(x))) == render(x).
    #[test]
    fn md_rendering_is_idempotent_and_stable() {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        let spec: AppSpec = serde_json::from_str(raw).expect("reference spec parses");
        let first = spec_to_markdown(&spec);
        let reparsed = markdown_to_spec(&first).expect("parses");
        let second = spec_to_markdown(&reparsed);
        assert_eq!(first, second);
        // A quick determinism check at the section level.
        assert!(first.starts_with("# AppSpec: spire-gis\n"));
    }

    #[test]
    fn md_errors_are_descriptive() {
        // Missing '# AppSpec:' header.
        assert!(markdown_to_spec("## UI\n### x\nLayout: spacer\n").is_err());
        // Layout line omitted for a screen.
        let md = "# AppSpec: X\n\n**Goal**: y\n\n## UI\n\n### map — Map\n";
        assert!(markdown_to_spec(md).unwrap_err().contains("Layout"));
        // Layout with a junk node kind.
        let md =
            "# AppSpec: X\n\n**Goal**: y\n\n## UI\n\n### map — Map\nLayout: frobnicate(\"x\")\n";
        assert!(markdown_to_spec(md)
            .unwrap_err()
            .contains("unknown layout kind"));
        // A hand-edited record table with a bad type errors, not misparses.
        let md = concat!(
            "# AppSpec: X\n\n**Goal**: y\n\n## Data types\n\n",
            "### `Broken` (record)\n| field | type |\n|---|---|\n",
            "| a | list< |\n"
        );
        assert!(markdown_to_spec(md)
            .unwrap_err()
            .contains("expected an identifier"));
    }
}
