// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! VSCode tool definitions — static metadata for all VS Code API handler functions.
//!
//! These tools are registered in the `ToolsActor` under the server name
//! `"vscode-extension"`. When called, the coordinator forwards the request
//! to the VS Code extension via JSON-RPC over stdout, and the extension
//! handles it via its local `Router` (which has all the handler modules
//! registered: workspace, document, diagnostics, git, symbols).
//!
//! This file is the single source of truth for VSC tool metadata.
//! The extension side (`spire-extension/src/extension.ts`) sends an identical
//! list at startup to keep both sides in sync.

use spire_core::actors::ToolInfo;

/// Return the list of all VS Code API tools with their metadata.
///
/// These are static — they don't change at runtime.
pub fn vscode_tool_definitions() -> Vec<ToolInfo> {
    vec![
        // ── Workspace tools ──
        ToolInfo {
            name: "workspace/getFolders".to_string(),
            description: "Get the list of workspace folders currently open in VS Code".to_string(),
            input_schema: json_schema!({}),
        },
        ToolInfo {
            name: "workspace/searchFiles".to_string(),
            description: "Search for files in the workspace using a glob pattern".to_string(),
            input_schema: json_schema!({
                "pattern": {"type": "string", "description": "Glob pattern to search for files"},
                "options": {
                    "type": "object",
                    "properties": {
                        "include": {"type": "string", "description": "Include pattern"},
                        "exclude": {"type": "string", "description": "Exclude pattern"}
                    }
                }
            }),
        },
        ToolInfo {
            name: "workspace/searchText".to_string(),
            description: "Search for text patterns across files in the workspace".to_string(),
            input_schema: json_schema!({
                "pattern": {"type": "string", "description": "Text or regex pattern to search"},
                "options": {
                    "type": "object",
                    "properties": {
                        "include": {"type": "string", "description": "File include pattern"},
                        "maxResults": {"type": "integer", "description": "Maximum results"},
                        "contextLines": {"type": "integer", "description": "Context lines per match"}
                    }
                }
            }),
        },
        // ── Document tools ──
        ToolInfo {
            name: "document/read".to_string(),
            description: "Read the contents of a document by URI".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "options": {
                    "type": "object",
                    "properties": {
                        "startLine": {"type": "integer", "description": "Start line (0-based)"},
                        "endLine": {"type": "integer", "description": "End line (exclusive)"}
                    }
                }
            }),
        },
        ToolInfo {
            name: "document/insertText".to_string(),
            description: "Insert text at a specific position in a document".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "position": {
                    "type": "object",
                    "properties": {
                        "line": {"type": "integer"},
                        "character": {"type": "integer"}
                    },
                    "required": ["line", "character"]
                },
                "text": {"type": "string", "description": "Text to insert"}
            }),
        },
        ToolInfo {
            name: "document/replaceText".to_string(),
            description: "Replace text in a range within a document".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "range": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "object", "properties": {"line": {"type": "integer"}, "character": {"type": "integer"}}},
                        "end": {"type": "object", "properties": {"line": {"type": "integer"}, "character": {"type": "integer"}}}
                    }
                },
                "text": {"type": "string", "description": "Replacement text"}
            }),
        },
        ToolInfo {
            name: "document/deleteRange".to_string(),
            description: "Delete text in a range within a document".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "range": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "object", "properties": {"line": {"type": "integer"}, "character": {"type": "integer"}}},
                        "end": {"type": "object", "properties": {"line": {"type": "integer"}, "character": {"type": "integer"}}}
                    }
                }
            }),
        },
        ToolInfo {
            name: "document/format".to_string(),
            description: "Format a document using VS Code's formatter".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"}
            }),
        },
        ToolInfo {
            name: "document/applyEdit".to_string(),
            description: "Apply a series of text edits to a document".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "range": {"type": "object"},
                            "newText": {"type": "string"}
                        }
                    }
                }
            }),
        },
        // ── Diagnostics tools ──
        ToolInfo {
            name: "diagnostics/get".to_string(),
            description: "Get diagnostics (errors, warnings) for files in the workspace"
                .to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Optional URI to filter by"},
                "severity": {"type": "string", "description": "Optional severity filter: error, warning, information, hint"}
            }),
        },
        // ── Git tools ──
        ToolInfo {
            name: "git/getChanges".to_string(),
            description: "Get the current git working tree and staged changes".to_string(),
            input_schema: json_schema!({
                "staged": {"type": "boolean", "description": "Filter by staged status"},
                "uri": {"type": "string", "description": "Optional URI to filter by"}
            }),
        },
        // ── Symbol / Code Intelligence tools ──
        ToolInfo {
            name: "symbols/goToDefinition".to_string(),
            description: "Go to the definition of a symbol at a given position".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "position": {
                    "type": "object",
                    "properties": {
                        "line": {"type": "integer"},
                        "character": {"type": "integer"}
                    },
                    "required": ["line", "character"]
                }
            }),
        },
        ToolInfo {
            name: "symbols/findReferences".to_string(),
            description: "Find all references to a symbol at a given position".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "position": {
                    "type": "object",
                    "properties": {
                        "line": {"type": "integer"},
                        "character": {"type": "integer"}
                    },
                    "required": ["line", "character"]
                }
            }),
        },
        ToolInfo {
            name: "symbols/getHover".to_string(),
            description: "Get hover information for a symbol at a given position".to_string(),
            input_schema: json_schema!({
                "uri": {"type": "string", "description": "Document URI"},
                "position": {
                    "type": "object",
                    "properties": {
                        "line": {"type": "integer"},
                        "character": {"type": "integer"}
                    },
                    "required": ["line", "character"]
                }
            }),
        },
        // ── RAG tools ──
        ToolInfo {
            name: "rag/search".to_string(),
            description: "Semantic search over the platform RAG corpus (chunks) for a domain/machine model. Returns ranked chunks with source paths and scores.".to_string(),
            input_schema: json_schema!({
                "domain": { "type": "string", "required": false, "description": "Platform domain (e.g. a7s); when omitted the currently-selected platform is used" },
                "query": { "type": "string", "required": true },
                "top_k": { "type": "integer", "required": false }
            }),
        },
        ToolInfo {
            name: "rag/find-interfaces".to_string(),
            description: "Semantic search over code-only RAG sources (HAL functions/APIs) for a platform domain. Returns ranked code chunks.".to_string(),
            input_schema: json_schema!({
                "domain": { "type": "string", "required": false, "description": "Platform domain (e.g. a7s); when omitted the currently-selected platform is used" },
                "query": { "type": "string", "required": true },
                "top_k": { "type": "integer", "required": false }
            }),
        },
        ToolInfo {
            name: "rag/set-domain".to_string(),
            description: "Set the default RAG domain so later rag/search and rag/find-interfaces calls can omit the 'domain' argument (human UI selection also updates this).".to_string(),
            input_schema: json_schema!({
                "domain": { "type": "string", "required": true, "description": "Platform domain (e.g. a7s)" }
            }),
        },
        ToolInfo {
            name: "rag/list-domains".to_string(),
            description: "List available RAG platform domains with their corpus stats (chunks, entities).".to_string(),
            input_schema: json_schema!({}),
        },
    ]
}

/// Helper macro to build a JSON schema with a `required` array derived from
/// properties that have `"required": true` set.
macro_rules! json_schema {
    ($props:tt) => {{
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": serde_json::json!($props),
            "required": []
        });
        // Collect required property names
        if let Some(props) = schema["properties"].as_object() {
            let required: Vec<String> = props.iter()
                .filter(|(_, v)| v.get("required").and_then(|r| r.as_bool()).unwrap_or(false))
                .map(|(k, _)| k.clone())
                .collect();
            schema["required"] = serde_json::json!(required);
            // Remove the internal "required" flag from each property
            if let Some(props) = schema["properties"].as_object_mut() {
                for (_, v) in props.iter_mut() {
                    if let Some(obj) = v.as_object_mut() {
                        obj.remove("required");
                    }
                }
            }
        }
        schema
    }};
}

// Re-export the macro so it's available in this module
pub(crate) use json_schema;
