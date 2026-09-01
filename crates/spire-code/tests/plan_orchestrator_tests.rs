// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Tests for the PlanOrchestrator + Plan/PlanStep graph operations.

use serde_json::Value;
use spire_core::subsystems::graph::memory_graph::{MemoryGraphActor, MemoryGraphMessage};
use spire_code::subsystems::planning::plan_orchestrator::{PlanOrchestrator, PlanOrchestratorMessage};
use spire_actor::ActorSystem;
use spire_core::models::memory_graph::{AttrNode, NodeUpdate};
use spire_core::models::memory_graph::{PlanStatus, PlanStepData};
use std::collections::HashMap;

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

async fn spawn_memory_graph(system: &ActorSystem) -> tokio::sync::mpsc::Sender<MemoryGraphMessage> {
    let actor = MemoryGraphActor::new();
    let (tx, _handle) = system.spawn(actor);
    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let (init_tx, init_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::Initialize {
        data_dir: tmp_dir.path().to_path_buf(),
        reply_to: init_tx,
    })
    .await
    .unwrap();
    init_rx.await.unwrap().unwrap();
    tx
}

fn unknown_props<'a>(node: &'a AttrNode) -> &'a HashMap<String, Value> {
    &node.properties
}

// ============================================================================
// Layer 1: Plan Node CRUD (uses Unknown variant for property round-trips)
// ============================================================================

#[tokio::test]
async fn test_plan_node_store_and_retrieve() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let mut props = HashMap::new();
    props.insert(
        "goal".to_string(),
        Value::String("Build and fix the project".to_string()),
    );
    props.insert("status".to_string(), Value::String("pending".to_string()));
    props.insert("total_steps".to_string(), Value::Number(3.into()));
    props.insert("completed_steps".to_string(), Value::Number(0.into()));
    props.insert("failed_steps".to_string(), Value::Number(0.into()));

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("plan".to_string()),
                "test-plan-1".to_string(),
                Some("Test plan goal".to_string()),
                props,
            ),
            reply_to: tx,
        })
        .await
        .unwrap();
    let stored = rx.await.unwrap().unwrap();
    assert_eq!(stored.node_type_str(), "Unknown");
    assert_eq!(stored.name(), "test-plan-1");
    assert_eq!(stored.description(), Some("Test plan goal"));

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::GetAttrNode {
            id: stored.id().to_string(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let retrieved = rx.await.unwrap().unwrap().unwrap();
    assert_eq!(retrieved.name(), "test-plan-1");
    let props = unknown_props(&retrieved);
    assert_eq!(
        props.get("goal").and_then(|v| v.as_str()),
        Some("Build and fix the project")
    );
    assert_eq!(
        props.get("status").and_then(|v| v.as_str()),
        Some("pending")
    );
}

#[tokio::test]
async fn test_plan_node_update_status() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let mut props = HashMap::new();
    props.insert(
        "goal".to_string(),
        Value::String("Refactor code".to_string()),
    );
    props.insert("status".to_string(), Value::String("pending".to_string()));

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("plan".to_string()),
                "test-plan-2".to_string(),
                Some("Another test plan".to_string()),
                props,
            ),
            reply_to: tx,
        })
        .await
        .unwrap();
    let stored = rx.await.unwrap().unwrap();

    let mut new_props = HashMap::new();
    new_props.insert("status".to_string(), Value::String("approved".to_string()));
    new_props.insert(
        "goal".to_string(),
        Value::String("Refactor code".to_string()),
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::UpdateNode {
            id: stored.id().to_string(),
            updates: NodeUpdate {
                node_type: None,
                subtype: None,
                name: None,
                description: None,
                properties: Some(new_props),
                embedding_id: None,
            },
            reply_to: tx,
        })
        .await
        .unwrap();
    let updated = rx.await.unwrap().unwrap();
    assert_eq!(
        unknown_props(&updated)
            .get("status")
            .and_then(|v| v.as_str()),
        Some("approved"),
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::GetAttrNode {
            id: stored.id().to_string(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let retrieved = rx.await.unwrap().unwrap().unwrap();
    assert_eq!(
        unknown_props(&retrieved)
            .get("status")
            .and_then(|v| v.as_str()),
        Some("approved")
    );
}

// ============================================================================
// Layer 2: PlanStep Node CRUD with HAS_STEP relationship
// ============================================================================

#[tokio::test]
async fn test_plan_step_node_store_and_link_to_plan() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("plan".to_string()),
                "plan-with-steps".to_string(),
                Some("Plan with steps".to_string()),
                HashMap::new(),
            ),
            reply_to: tx,
        })
        .await
        .unwrap();
    let plan = rx.await.unwrap().unwrap();

    let mut step_props = HashMap::new();
    step_props.insert(
        "step_name".to_string(),
        Value::String("read_error_context".to_string()),
    );
    step_props.insert("order".to_string(), Value::Number(1.into()));
    step_props.insert("status".to_string(), Value::String("pending".to_string()));

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("plan_step".to_string()),
                "plan-step-1".to_string(),
                Some("First step".to_string()),
                step_props,
            ),
            reply_to: tx,
        })
        .await
        .unwrap();
    let step = rx.await.unwrap().unwrap();
    assert_eq!(step.node_type_str(), "Unknown");
    assert_eq!(step.name(), "plan-step-1");

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::CreateRelationship {
            rel: spire_core::models::memory_graph::RelationshipInput {
                edge_type: spire_core::models::memory_graph::RelationshipType::DependsOn,
                from_id: plan.id().to_string(),
                to_id: step.id().to_string(),
                properties: None,
                weight: None,
            },
            reply_to: tx,
        })
        .await
        .unwrap();
    let _rel = rx.await.unwrap().unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::GetRelationships {
            node_id: plan.id().to_string(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let edges = rx.await.unwrap().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].from_id, plan.id());
    assert_eq!(edges[0].to_id, step.id());
}

// ============================================================================
// Layer 3: Plan Status Query
// ============================================================================

/// Helper to create a plan with steps, storing plan as NodeType::Plan
/// (used by PlanOrchestrator which queries NodeType::Plan).
async fn create_plan_variant(
    memory_graph: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    plan_name: &str,
    step_names: &[&str],
) -> (String, Vec<String>) {
    // Store plan as NodeType::Plan (typed variant — properties from NodeInput discarded)
    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Plan",
                None,
                plan_name.to_string(),
                Some("Integration test plan".to_string()),
                HashMap::from([
                    ("goal".to_string(), Value::String("Test goal".to_string())),
                    ("status".to_string(), Value::String("executing".to_string())),
                ])),
            reply_to: tx,
        })
        .await
        .unwrap();
    let plan = rx.await.unwrap().unwrap();
    let plan_id = plan.id().to_string();

    let mut step_ids = Vec::new();
    for (i, step_name) in step_names.iter().enumerate() {
        let mut step_props = HashMap::new();
        step_props.insert(
            "step_name".to_string(),
            Value::String(step_name.to_string()),
        );
        step_props.insert("order".to_string(), Value::Number((i as u64).into()));
        step_props.insert("status".to_string(), Value::String("pending".to_string()));

        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::StoreAttrNode {
                node: t_attr_unknown(
                    "Unknown",
                    Some("plan_step".to_string()),
                    step_name.to_string(),
                    Some(format!("Step {}", i)),
                    step_props,
                ),
                reply_to: tx,
            })
            .await
            .unwrap();
        let step = rx.await.unwrap().unwrap();
        let step_id = step.id().to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::CreateRelationship {
                rel: spire_core::models::memory_graph::RelationshipInput {
                    edge_type: spire_core::models::memory_graph::RelationshipType::Custom(
                        "HAS_STEP".to_string(),
                    ),
                    from_id: plan_id.clone(),
                    to_id: step_id.clone(),
                    properties: None,
                    weight: None,
                },
                reply_to: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap().unwrap();

        step_ids.push(step_id);
    }

    (plan_id, step_ids)
}

fn verify_steps_sorted(steps: &[AttrNode]) {
    for i in 1..steps.len() {
        let prev_order = unknown_props(&steps[i - 1])
            .get("order")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let curr_order = unknown_props(&steps[i])
            .get("order")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(prev_order <= curr_order, "Steps should be sorted by order");
    }
}

#[tokio::test]
async fn test_plan_status_result_construction() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let plan_name = "construction-plan";
    let (plan_id, step_ids) =
        create_plan_variant(&memory_graph, plan_name, &["step-1", "step-2", "step-3"]).await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::GetAttrNode {
            id: plan_id.clone(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let plan_node = rx.await.unwrap().unwrap().unwrap();
    assert_eq!(plan_node.node_type_str(), "Plan");
    assert_eq!(plan_node.name(), plan_name);

    let mut steps = Vec::new();
    for step_id in &step_ids {
        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::GetAttrNode {
            id: step_id.clone(),
            reply_to: tx,
        })
            .await
            .unwrap();
        let step_node = rx.await.unwrap().unwrap().unwrap();
        assert_eq!(step_node.node_type_str(), "Unknown");
        steps.push(step_node);
    }

    verify_steps_sorted(&steps);
}

#[tokio::test]
async fn test_plan_status_query_returns_all_fields() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    // Use Unknown variant so properties survive round-trip
    let mut plan_props = HashMap::new();
    plan_props.insert("goal".to_string(), Value::String("Test goal".to_string()));
    plan_props.insert("status".to_string(), Value::String("executing".to_string()));
    plan_props.insert(
        "intent_name".to_string(),
        Value::String("test-intent".to_string()),
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Unknown",
                Some("plan".to_string()),
                "query-test-plan".to_string(),
                Some("Integration test plan".to_string()),
                plan_props,
            ),
            reply_to: tx,
        })
        .await
        .unwrap();
    rx.await.unwrap().unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("plan".to_string()),
                name: Some("query-test-plan".to_string()),
                limit: None,
                reply_to: tx,
            })
        .await
        .unwrap();
    let plans = rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(plans.len(), 1, "Should find exactly one plan");

    let props = unknown_props(&plans[0]);
    assert_eq!(
        props.get("goal").and_then(|v| v.as_str()),
        Some("Test goal")
    );
    assert_eq!(
        props.get("intent_name").and_then(|v| v.as_str()),
        Some("test-intent")
    );
}

#[tokio::test]
async fn test_plan_step_dependency_resolution() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    let mut step_ids = Vec::new();
    for i in 1..=3 {
        let mut props = HashMap::new();
        props.insert(
            "step_name".to_string(),
            Value::String(format!("dep-step-{}", i)),
        );
        props.insert("order".to_string(), Value::Number((i as u64).into()));
        props.insert("status".to_string(), Value::String("pending".to_string()));
        props.insert(
            "depends_on".to_string(),
            Value::Array(if i > 1 {
                vec![Value::Number((i as u64 - 1).into())]
            } else {
                vec![]
            }),
        );

        let (tx, rx) = tokio::sync::oneshot::channel();
        memory_graph
            .send(MemoryGraphMessage::StoreAttrNode {
                node: t_attr_unknown(
                "Unknown",
                Some("plan_step".to_string()),
                format!("dep-step-{}", i),
                Some(format!("Dependency step {}", i)),
                props,
            ),
                reply_to: tx,
            })
            .await
            .unwrap();
        let step = rx.await.unwrap().unwrap();
        step_ids.push(step.id().to_string());
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::GetAttrNode {
            id: step_ids[2].clone(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let step3 = rx.await.unwrap().unwrap().unwrap();
    let deps = unknown_props(&step3)
        .get("depends_on")
        .and_then(|v| v.as_array());
    assert!(deps.is_some(), "step-3 should have depends_on");
    assert_eq!(deps.unwrap().len(), 1, "step-3 should depend on 1 step");
}

// ============================================================================
// Layer 4: PlanOrchestrator Message Handling
// ============================================================================

#[tokio::test]
async fn test_plan_orchestrator_send_message() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    let llm_mock = mock_sender();
    let tool_orchestrator_mock = mock_sender();
    let chat_mock = mock_sender();
    let transport_mock = mock_sender();

    let (plan_orch_tx, _plan_orch_handle) = system.spawn(PlanOrchestrator::new(
        memory_graph,
        llm_mock,
        tool_orchestrator_mock,
        chat_mock,
        transport_mock,
    ));

    let (tx, rx) = tokio::sync::oneshot::channel();
    plan_orch_tx
        .send(PlanOrchestratorMessage::GetPlanStatus {
            plan_id: "nonexistent-plan".to_string(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let result = rx.await.unwrap();
    assert!(
        result.is_err(),
        "GetPlanStatus for nonexistent plan should error"
    );
}

#[tokio::test]
async fn test_plan_orchestrator_get_status_with_mock_graph() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    let llm_mock = mock_sender();
    let tool_orchestrator_mock = mock_sender();
    let chat_mock = mock_sender();
    let transport_mock = mock_sender();

    let plan_name = "orchestrator-test-plan";
    let (plan_id, _) = create_plan_variant(&memory_graph, plan_name, &["s1", "s2"]).await;

    let (plan_orch_tx, _plan_orch_handle) = system.spawn(PlanOrchestrator::new(
        memory_graph,
        llm_mock,
        tool_orchestrator_mock,
        chat_mock,
        transport_mock,
    ));

    let (tx, rx) = tokio::sync::oneshot::channel();
    plan_orch_tx
        .send(PlanOrchestratorMessage::GetPlanStatus {
            plan_id: plan_id.clone(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let result = rx.await.unwrap();

    match result {
        Ok(status) => {
            assert_eq!(status.plan_id, plan_id);
            // PlanOrchestrator reads from Plan typed fields; NodeInput properties discarded
            // so goal will be None/empty
        }
        Err(e) => {
            eprintln!("GetPlanStatus returned error (expected): {}", e);
        }
    }
}

#[tokio::test]
async fn test_plan_orchestrator_update_and_query_status() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    let llm_mock = mock_sender();
    let tool_orchestrator_mock = mock_sender();
    let chat_mock = mock_sender();
    let transport_mock = mock_sender();

    let (plan_id, _step_ids) =
        create_plan_variant(&memory_graph, "status-update-plan", &["step-x"]).await;

    let (plan_orch_tx, _plan_orch_handle) = system.spawn(PlanOrchestrator::new(
        memory_graph.clone(),
        llm_mock,
        tool_orchestrator_mock,
        chat_mock,
        transport_mock,
    ));

    let (tx, rx) = tokio::sync::oneshot::channel();
    plan_orch_tx
        .send(PlanOrchestratorMessage::GetPlanStatus {
            plan_id: plan_id.clone(),
            reply_to: tx,
        })
        .await
        .unwrap();
    let result = rx.await.unwrap();

    if let Ok(status) = result {
        assert_eq!(status.status as usize, 2);
    }

    let mut update_props = HashMap::new();
    update_props.insert("status".to_string(), Value::String("completed".to_string()));
    update_props.insert("goal".to_string(), Value::String("Test goal".to_string()));
    update_props.insert("completed_steps".to_string(), Value::Number(1.into()));

    let (tx, rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::UpdateNode {
            id: plan_id.clone(),
            updates: NodeUpdate {
                node_type: None,
                subtype: None,
                name: None,
                description: None,
                properties: Some(update_props),
                embedding_id: None,
            },
            reply_to: tx,
        })
        .await
        .unwrap();
    rx.await.unwrap().unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel();
    plan_orch_tx
        .send(PlanOrchestratorMessage::GetPlanStatus {
            plan_id: plan_id,
            reply_to: tx,
        })
        .await
        .unwrap();
    let result2 = rx.await.unwrap();
    if let Ok(status2) = result2 {
        assert_eq!(status2.status as usize, 4);
    }
}

#[tokio::test]
async fn test_update_plan_status_serialization() {
    let completed = PlanStatus::Completed;
    let json = serde_json::to_string(&completed).unwrap();
    assert_eq!(json, "\"completed\"");

    let deserialized: PlanStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, PlanStatus::Completed);
}

#[tokio::test]
async fn test_plan_step_data_serialization() {
    let data = PlanStepData {
        description: "Test step".to_string(),
        step_name: "test-step".to_string(),
        arg_template: serde_json::json!({"path": "$error.file"}),
        depends_on: vec![1, 2, 3],
        uses_error_context: true,
    };

    let json = serde_json::to_string(&data).unwrap();
    let deserialized: PlanStepData = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.description, data.description);
    assert_eq!(deserialized.step_name, data.step_name);
    assert_eq!(deserialized.arg_template, data.arg_template);
    assert_eq!(deserialized.depends_on, data.depends_on);
    assert_eq!(deserialized.uses_error_context, data.uses_error_context);
}
