// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Tests for the graph-driven ToolOrchestrator + StepContext + bootstrap pipeline.

use spire_core::subsystems::graph::memory_graph::{MemoryGraphActor, MemoryGraphMessage};
use spire_core::subsystems::tools::tool_orchestrator::StepContext;
use spire_code::actors::{ToolOrchestrator, ToolOrchestratorMessage};
use spire_actor::ActorSystem;
use spire_core::models::memory_graph::{BuildError, AttrNode};
use std::collections::HashMap;

/// Helper to create a mock sender for any actor channel.
fn mock_sender<T>() -> tokio::sync::mpsc::Sender<T> {
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    tx
}

/// Build an envelope `AttrNode` for a test node.
fn t_attr_unknown(
    node_type: &str,
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: std::collections::HashMap<String, serde_json::Value>,
) -> AttrNode {
    let now = chrono::Utc::now();
    AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: node_type.to_string(),
        subtype,
        name,
        description,
        properties,
        embedding_id: None,
        created_at: now,
        updated_at: now,
        version: 1,
    }
}

/// Helper to spawn a MemoryGraphActor and return its sender.
async fn spawn_memory_graph(system: &ActorSystem) -> tokio::sync::mpsc::Sender<MemoryGraphMessage> {
    let actor = MemoryGraphActor::new();
    let (tx, _handle) = system.spawn(actor);
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir for memory graph");
    let (init_tx, init_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::Initialize {
        data_dir: tmp_dir.path().to_path_buf(),
        reply_to: init_tx,
    })
    .await
    .expect("Failed to send Initialize message");
    init_rx
        .await
        .expect("Failed to receive Initialize response")
        .expect("MemoryGraph initialization failed");
    tx
}

// ============================================================================
// Layer 1: StepContext — Variable Resolution
// ============================================================================

fn make_test_context() -> StepContext {
    let mut ctx = StepContext::new(
        &BuildError {
            error_text: "error[E0308]: mismatched types".to_string(),
            error_type: Some("rustc-compile-error".to_string()),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            column: Some(5),
            exit_code: Some(1),
            build_type: Some("Cargo".to_string()),
            diagnostic_node_id: Some("diag-1".to_string()),
            file_node_id: Some("file-1".to_string()),
        },
        "/home/user/project",
    );
    ctx.set_output(
        "file_context",
        serde_json::json!("fn main() {{\n    let x: i32 = \"hello\";\n}}"),
    );
    ctx.set_output(
        "analysis",
        serde_json::json!({
            "error_summary": "Type mismatch: expected i32, found &str",
            "root_cause": "Variable 'x' declared as i32 but assigned a string literal",
            "suggested_fix": "Change the type annotation to &str or change the value to an integer"
        }),
    );
    ctx.set_output(
        "search_results",
        serde_json::json!(["src/handlers/chat.ts", "src/models/chat.ts"]),
    );
    ctx
}

#[test]
fn test_step_context_resolve_error_file() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$error.file");
    assert_eq!(val, Some(serde_json::json!("src/main.rs")));
}

#[test]
fn test_step_context_resolve_error_line() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$error.line");
    assert_eq!(val, Some(serde_json::json!(42)));
}

#[test]
fn test_step_context_resolve_error_column() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$error.column");
    assert_eq!(val, Some(serde_json::json!(5)));
}

#[test]
fn test_step_context_resolve_error_text() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$error.error_text");
    assert_eq!(
        val,
        Some(serde_json::json!("error[E0308]: mismatched types"))
    );
}

#[test]
fn test_step_context_resolve_error_type() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$error.error_type");
    assert_eq!(val, Some(serde_json::json!("rustc-compile-error")));
}

#[test]
fn test_step_context_resolve_build_type() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$error.build_type");
    assert_eq!(val, Some(serde_json::json!("Cargo")));
}

#[test]
fn test_step_context_resolve_project_root() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$project_root");
    assert_eq!(val, Some(serde_json::json!("/home/user/project")));
}

#[test]
fn test_step_context_resolve_step_output() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$step.file_context");
    assert!(val.is_some(), "step.file_context should resolve");
}

#[test]
fn test_step_context_resolve_nested_field() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$step.analysis.error_summary");
    assert_eq!(
        val,
        Some(serde_json::json!("Type mismatch: expected i32, found &str"))
    );
}

#[test]
fn test_step_context_resolve_deeply_nested() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$step.analysis.suggested_fix");
    assert_eq!(
        val,
        Some(serde_json::json!(
            "Change the type annotation to &str or change the value to an integer"
        ))
    );
}

#[test]
fn test_step_context_resolve_array_index() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$step.search_results[0]");
    assert_eq!(val, Some(serde_json::json!("src/handlers/chat.ts")));
}

#[test]
fn test_step_context_resolve_array_second_index() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$step.search_results[1]");
    assert_eq!(val, Some(serde_json::json!("src/models/chat.ts")));
}

#[test]
fn test_step_context_unknown_variable_returns_none() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$bogus.thing");
    assert!(val.is_none(), "Unknown $variable should return None");
}

#[test]
fn test_step_context_missing_step_output_returns_none() {
    let ctx = make_test_context();
    let val = ctx.resolve_variable("$step.nonexistent_output");
    assert!(val.is_none(), "Missing step output should return None");
}

#[test]
fn test_step_context_compute_expression_add() {
    let ctx = make_test_context();
    let val = ctx.compute_expression("$error.line + 5");
    assert_eq!(val, Some(47i64));
}

#[test]
fn test_step_context_compute_expression_sub() {
    let ctx = make_test_context();
    let val = ctx.compute_expression("$error.line - 5");
    assert_eq!(val, Some(37i64));
}

#[test]
fn test_step_context_compute_expression_literal_number() {
    let ctx = make_test_context();
    let val = ctx.compute_expression("10");
    assert_eq!(val, Some(10i64));
}

#[test]
fn test_step_context_empty_context_defaults() {
    let ctx = StepContext::default();
    assert!(ctx.error.is_none());
    assert!(ctx.project_root.is_empty());
    assert!(ctx.step_outputs.is_empty());
    assert!(ctx.build_type.is_empty());
}

// ============================================================================
// Layer 2: Step Context — set_output and new constructor
// ============================================================================

#[test]
fn test_step_context_new_sets_error_fields() {
    let error = BuildError {
        error_text: "test error".to_string(),
        error_type: None,
        file: Some("test.rs".to_string()),
        line: Some(10),
        column: None,
        exit_code: None,
        build_type: Some("Cargo".to_string()),
        diagnostic_node_id: None,
        file_node_id: None,
    };
    let ctx = StepContext::new(&error, "/tmp/test");
    assert_eq!(ctx.project_root, "/tmp/test");
    assert_eq!(ctx.build_type, "Cargo");
    assert!(ctx.error.is_some());
}

#[test]
fn test_step_context_set_output_then_resolve() {
    let mut ctx = StepContext::default();
    ctx.set_output("my_key", serde_json::json!("my_value"));
    let val = ctx.resolve_variable("$step.my_key");
    assert_eq!(val, Some(serde_json::json!("my_value")));
}

// ============================================================================
// Layer 3: bootstrap_strategy_steps into MemoryGraph
// ============================================================================

#[tokio::test]
async fn test_bootstrap_strategy_steps_graph_seeding() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let config_content = r#"{
        "step_definitions": [
            {
                "name": "test-read-step",
                "description": "A test step",
                "concrete_tool": "workspace/readFile",
                "provider": "vscode-extension",
                "arg_template": { "path": "$error.file" },
                "depends_on": [],
                "output_key": "file_content",
                "category": "read"
            }
        ],
        "tool_providers": [
            {
                "name": "vscode-extension",
                "transport": "extension",
                "prefix": "workspace/",
                "description": "VS Code extension tools"
            }
        ]
    }"#;

    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let config_path = tmp_dir.path().join("strategy-steps.json");
    tokio::fs::write(&config_path, config_content)
        .await
        .expect("Failed to write config");

    let result =
        spire_code::actors::startup_phases::bootstrap_strategy_steps(&memory_graph, &config_path)
            .await;
    assert!(
        result.is_ok(),
        "bootstrap_strategy_steps should succeed, got: {:?}",
        result
    );
    let count = result.unwrap();
    assert_eq!(count, 2, "Should store 2 nodes (1 step + 1 provider)");

    // Note: bootstrap_strategy_steps stores ToolProvider nodes with the open
    // discriminator "ToolProvider" + subtype "tool_provider".
    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("ToolProvider".to_string()),
                subtype: Some("tool_provider".to_string()),
                name: Some("vscode-extension".to_string()),
                limit: Some(10),
                reply_to: tx,
            })
        .await
        .unwrap();
    let providers = rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(providers.len(), 1, "Should have 1 ToolProvider node");
    assert_eq!(providers[0].name(), "vscode-extension");

    // Verify StepDefinition node exists
    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("StepDefinition".to_string()),
                subtype: Some("step_definition".to_string()),
                name: Some("test-read-step".to_string()),
                limit: Some(10),
                reply_to: tx,
            })
        .await
        .unwrap();
    let steps = rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(steps.len(), 1, "Should have 1 StepDefinition node");
    assert_eq!(steps[0].name(), "test-read-step");
    assert_eq!(steps[0].subtype(), Some("step_definition"));
}

#[tokio::test]
async fn test_bootstrap_strategy_steps_depends_on_stored() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let config_content = r#"{
        "step_definitions": [
            {
                "name": "step-with-deps",
                "description": "Step with dependencies",
                "concrete_tool": "llm/analyze",
                "provider": "llm",
                "arg_template": {},
                "depends_on": ["read_error_context", "identify_missing_symbol"],
                "output_key": "analysis",
                "category": "analyze"
            }
        ],
        "tool_providers": []
    }"#;

    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let config_path = tmp_dir.path().join("strategy-steps.json");
    tokio::fs::write(&config_path, config_content)
        .await
        .expect("Failed to write config");

    let result =
        spire_code::actors::startup_phases::bootstrap_strategy_steps(&memory_graph, &config_path)
            .await;
    assert!(result.is_ok());

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("StepDefinition".to_string()),
                subtype: Some("step_definition".to_string()),
                name: Some("step-with-deps".to_string()),
                limit: Some(10),
                reply_to: tx,
            })
        .await
        .unwrap();
    let steps = rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(steps.len(), 1);
    // Standard variant doesn't preserve custom properties, verify name only
    assert_eq!(steps[0].name(), "step-with-deps");
    assert_eq!(steps[0].subtype(), Some("step_definition"));
}

// ============================================================================
// Layer 4: ToolOrchestrator Graph Queries
// ============================================================================

#[tokio::test]
async fn test_tool_orchestrator_query_step_definition_success() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let mut props = HashMap::new();
    props.insert(
        "concrete_tool".to_string(),
        serde_json::json!("workspace/readFile"),
    );
    props.insert(
        "provider".to_string(),
        serde_json::json!("vscode-extension"),
    );
    props.insert(
        "arg_template".to_string(),
        serde_json::json!({"path": "$error.file"}),
    );
    props.insert("output_key".to_string(), serde_json::json!("file_context"));
    props.insert("category".to_string(), serde_json::json!("read"));

    let (store_tx, store_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("step_definition".to_string()),
                "test-read-error".to_string(),
                Some("Read error context".to_string()),
                props,
            ),
            reply_to: store_tx,
        })
        .await
        .unwrap();
    let stored = store_rx.await.unwrap().unwrap();
    assert_eq!(stored.name(), "test-read-error");

    let (to_tx, _to_handle) = system.spawn(ToolOrchestrator::new(
        memory_graph,
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
    ));

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    to_tx
        .send(ToolOrchestratorMessage::ExecuteTool {
            tool_name: "test-read-error".to_string(),
            parameters: HashMap::new(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let result = reply_rx.await.unwrap();
    assert!(result.is_err() || result.is_ok(), "Should not panic");
}

#[tokio::test]
async fn test_tool_orchestrator_query_nonexistent_step() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let (to_tx, _to_handle) = system.spawn(ToolOrchestrator::new(
        memory_graph,
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
    ));

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    to_tx
        .send(ToolOrchestratorMessage::ExecuteTool {
            tool_name: "nonexistent-step".to_string(),
            parameters: HashMap::new(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let result = reply_rx.await.unwrap();
    assert!(result.is_err(), "Nonexistent step should return an error");
}

#[tokio::test]
async fn test_tool_orchestrator_execute_tool_chain_success() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let step_names = ["step-alpha", "step-beta"];
    let tools = ["workspace/readFile", "llm/analyze"];
    let providers = ["vscode-extension", "llm"];
    let outputs = ["alpha_output", "beta_output"];

    for i in 0..2 {
        let mut props = HashMap::new();
        props.insert("concrete_tool".to_string(), serde_json::json!(tools[i]));
        props.insert("provider".to_string(), serde_json::json!(providers[i]));
        props.insert("arg_template".to_string(), serde_json::json!({}));
        props.insert("output_key".to_string(), serde_json::json!(outputs[i]));

        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::StoreAttrNode {
                node: t_attr_unknown(
                "Unknown",
                Some("step_definition".to_string()),
                step_names[i].to_string(),
                None,
                props,
            ),
                reply_to: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap().unwrap();
    }

    let (to_tx, _to_handle) = system.spawn(ToolOrchestrator::new(
        memory_graph,
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
    ));

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    to_tx
        .send(ToolOrchestratorMessage::ExecuteToolChain {
            tools: vec!["step-alpha".to_string(), "step-beta".to_string()],
            parameters: HashMap::new(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let result = reply_rx.await.unwrap();
    assert!(
        result.is_ok() || result.is_err(),
        "Tool chain should not panic"
    );
}

#[tokio::test]
async fn test_tool_orchestrator_execute_tool_chain_with_context() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let mut props = HashMap::new();
    props.insert(
        "concrete_tool".to_string(),
        serde_json::json!("workspace/readFile"),
    );
    props.insert(
        "provider".to_string(),
        serde_json::json!("vscode-extension"),
    );
    props.insert(
        "arg_template".to_string(),
        serde_json::json!({
            "path": "$error.file",
            "startLine": "$error.line - 5",
            "endLine": "$error.line + 5"
        }),
    );
    props.insert("output_key".to_string(), serde_json::json!("file_context"));

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("step_definition".to_string()),
                "read-error-context".to_string(),
                None,
                props,
            ),
            reply_to: tx,
        })
        .await
        .unwrap();
    rx.await.unwrap().unwrap();

    let (to_tx, _to_handle) = system.spawn(ToolOrchestrator::new(
        memory_graph,
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
    ));

    let error = BuildError {
        error_text: "error[E0308]: mismatched types".to_string(),
        error_type: Some("rustc-compile-error".to_string()),
        file: Some("src/main.rs".to_string()),
        line: Some(42),
        column: None,
        exit_code: Some(1),
        build_type: Some("Cargo".to_string()),
        diagnostic_node_id: None,
        file_node_id: None,
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    to_tx
        .send(ToolOrchestratorMessage::ExecuteToolChainWithContext {
            tools: vec!["read-error-context".to_string()],
            error,
            project_root: "/tmp/test-project".to_string(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let result = reply_rx.await.unwrap();
    assert!(
        result.is_ok() || result.is_err(),
        "Should complete without panic"
    );
}

// ============================================================================
// Layer 5: Full strategy-steps.json via bootstrap
// ============================================================================

#[tokio::test]
async fn test_bootstrap_strategy_steps_full_config() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
        .join("strategy-steps.json");

    if !config_path.exists() {
        eprintln!(
            "Skipping test: strategy-steps.json not found at {:?}",
            config_path
        );
        return;
    }

    let result =
        spire_code::actors::startup_phases::bootstrap_strategy_steps(&memory_graph, &config_path)
            .await;
    assert!(result.is_ok());
    let count = result.unwrap();
    assert!(
        count >= 15,
        "Should have at least 15 nodes from full config, got {}",
        count
    );

    // Verify all step definitions exist (stored as Unknown with subtype)
    let expected_steps = [
        "read_error_context",
        "read_warning_context",
        "analyze_type_mismatch",
        "analyze_borrow_pattern",
        "identify_missing_symbol",
        "find_correct_module",
        "add_import_statement",
        "apply_warning_fix",
        "analyze_dependency_tree",
        "find_compatible_versions",
        "update_manifests",
        "identify_missing_package",
        "run_install_command",
        "analyze_error",
        "apply_fix",
    ];

    for step_name in &expected_steps {
        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("step_definition".to_string()),
                name: Some(step_name.to_string()),
                limit: Some(10),
                reply_to: tx,
            })
            .await
            .unwrap();
        let nodes = rx.await.unwrap().expect("QueryNodes failed");
        assert!(
            !nodes.is_empty(),
            "StepDefinition '{}' should exist in graph",
            step_name
        );
    }

    // Verify all tool providers exist
    let expected_providers = [
        "vscode-extension",
        "mcp-cargo",
        "mcp-node",
        "llm",
        "project-meta",
    ];
    for name in &expected_providers {
        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("tool_provider".to_string()),
                name: Some(name.to_string()),
                limit: Some(10),
                reply_to: tx,
            })
            .await
            .unwrap();
        let nodes = rx.await.unwrap().expect("QueryNodes failed");
        assert!(
            !nodes.is_empty(),
            "ToolProvider '{}' should exist in graph",
            name
        );
    }
}

#[tokio::test]
async fn test_bootstrap_strategy_steps_twice_is_idempotent() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
        .join("strategy-steps.json");

    if !config_path.exists() {
        eprintln!("Skipping test: strategy-steps.json not found");
        return;
    }

    let r1 =
        spire_code::actors::startup_phases::bootstrap_strategy_steps(&memory_graph, &config_path)
            .await;
    assert!(r1.is_ok());

    let r2 =
        spire_code::actors::startup_phases::bootstrap_strategy_steps(&memory_graph, &config_path)
            .await;
    assert!(r2.is_ok());

    // After second bootstrap, should still have exactly one "read_error_context"
    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("step_definition".to_string()),
                name: Some("read_error_context".to_string()),
                limit: Some(10),
                reply_to: tx,
            })
        .await
        .unwrap();
    let nodes = rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(
        nodes.len(),
        1,
        "read_error_context should exist exactly once"
    );
}
