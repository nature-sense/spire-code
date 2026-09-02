// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **AppSpec requirements pass** — the LLM stage that turns a natural-language
//! goal into a **validated** [`AppSpec`].
//!
//! Flow (see [`generate_app_spec`]):
//!   1. Ask the planning LLM to emit an `AppSpec` JSON object. The schema
//!      guide + the canonical GIS example are embedded in the prompt, and the
//!      Spire framework API surface tells the LLM which backend subsystems
//!      real `uses`/types map to.
//!   2. Parse + [`validate`]. Warnings are non-blocking; error-severity
//!      issues gate the result.
//!   3. On any error, re-prompt with the concrete issue list and re-validate
//!      — up to [`MAX_ATTEMPTS`] total rounds (self-healing loop).
//!   4. Return the validated [`AppSpec`], or a typed [`SpecGenError`] if the
//!      LLM is unreachable, never emits parseable JSON, or the spec never
//!      validates.
//!
//! Nothing is written to disk here — codegen (the next stage) consumes the
//! validated spec.
//!
//! The LLM dependency is injected as a plain async `call_llm` closure, so the
//! unit tests run on canned responses (no live actor, no network).

use super::spec::{
    validate, ActorSpec, AppMeta, AppSpec, BridgeMethod, DomainType, Field, GraphEdgeType,
    GraphNodeType, GraphSchema, LayoutNode, Screen, SpecIssue, SpecIssueSeverity, Type, UiAction,
    UiBinding, UiNavigation,
};

/// Total LLM rounds (initial attempt + repairs) before giving up.
pub const MAX_ATTEMPTS: usize = 3;

/// Total critique + rewrite rounds applied AFTER a spec validates. The
/// improvement loop is bounded and best-effort (see [`generate_app_spec`]).
pub const MAX_IMPROVE_ROUNDS: usize = 2;

/// `validate()` plus the requirements-pass guard that `app.name` matches the
/// project name the prompt fixed.
fn validate_with_name(spec: &AppSpec, project_name: &str) -> Vec<SpecIssue> {
    let mut issues = validate(spec);
    if spec.app.name.trim().to_lowercase() != project_name.trim().to_lowercase() {
        issues.push(SpecIssue {
            severity: SpecIssueSeverity::Error,
            path: "app.name".to_string(),
            message: format!(
                "app.name must be '{project_name}' (projectName), got '{}'",
                spec.app.name
            ),
        });
    }
    issues
}

/// Improvement candidates must not regress the core surface area (bridge
/// methods + screens). Guards against degraded-but-valid rewrites replacing a
/// real spec.
fn is_substantive(candidate: &AppSpec, current: &AppSpec) -> bool {
    candidate.bridge.len() >= current.bridge.len() && candidate.ui.len() >= current.ui.len()
}

/// Failure of the AppSpec requirements pass.
#[derive(Debug)]
pub enum SpecGenError {
    /// The LLM could not be reached (not configured / actor gone / error).
    LlmUnavailable(String),
    /// The LLM answered but never produced parseable AppSpec JSON.
    Unparseable { attempts: usize, last_raw: String },
    /// A spec was parsed every round but still carried error-severity issues
    /// after [`MAX_ATTEMPTS`]. The last-best spec + remaining issues are kept
    /// so the caller can show the user exactly what is wrong.
    Invalid {
        attempts: usize,
        spec: AppSpec,
        issues: Vec<SpecIssue>,
    },
}

impl std::fmt::Display for SpecGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecGenError::LlmUnavailable(e) => {
                write!(f, "LLM unavailable during AppSpec requirements pass: {e}")
            }
            SpecGenError::Unparseable { attempts, last_raw } => {
                write!(
                    f,
                    "AppSpec requirements pass: LLM produced no parseable AppSpec JSON \
                     after {attempts} attempt(s) (last response starts: {})",
                    truncate(last_raw, 300)
                )
            }
            SpecGenError::Invalid {
                attempts,
                issues,
                spec,
            } => {
                write!(
                    f,
                    "AppSpec requirements pass: spec for '{}' still invalid after {attempts} \
                     attempt(s) with {} error(s):\n",
                    spec.app.name,
                    issues
                        .iter()
                        .filter(|i| i.severity == SpecIssueSeverity::Error)
                        .count()
                )?;
                for i in issues {
                    writeln!(
                        f,
                        "  [{}] {} — {}",
                        spec_severity_label(i),
                        i.path,
                        i.message
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SpecGenError {}

fn spec_severity_label(issue: &SpecIssue) -> &'static str {
    match issue.severity {
        SpecIssueSeverity::Error => "error",
        SpecIssueSeverity::Warning => "warning",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// The canonical GIS spec (also the JSON template embedded in the prompt).
pub(crate) fn example_gis_spec() -> AppSpec {
    AppSpec {
        app: AppMeta {
            name: "spire-gis".to_string(),
            goal: "view and edit map layers".to_string(),
        },
        graph: GraphSchema {
            nodes: vec![GraphNodeType {
                name: "map_layer".to_string(),
                description: "a rendered map layer".to_string(),
                fields: vec![
                    Field {
                        name: "id".to_string(),
                        ty: Type::Str,
                    },
                    Field {
                        name: "visible".to_string(),
                        ty: Type::Bool,
                    },
                ],
            }],
            edges: vec![GraphEdgeType {
                name: "contains".to_string(),
                from: "map_layer".to_string(),
                to: "map_layer".to_string(),
                description: String::new(),
            }],
        },
        types: vec![DomainType::Record {
            name: "LayerInfo".to_string(),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    ty: Type::Str,
                },
                Field {
                    name: "visible".to_string(),
                    ty: Type::Bool,
                },
            ],
        }],
        actors: vec![ActorSpec {
            name: "MapActor".to_string(),
            description: "holds layer state".to_string(),
            handlers: vec!["map/listLayers".to_string(), "map/addLayer".to_string()],
            state: vec![Field {
                name: "layers".to_string(),
                ty: Type::List {
                    of: Box::new(Type::Named {
                        name: "LayerInfo".to_string(),
                    }),
                },
            }],
            uses: vec!["build_types".to_string()],
        }],
        bridge: vec![
            BridgeMethod {
                method: "map/listLayers".to_string(),
                description: String::new(),
                params: vec![],
                result: Type::List {
                    of: Box::new(Type::Named {
                        name: "LayerInfo".to_string(),
                    }),
                },
            },
            BridgeMethod {
                method: "map/addLayer".to_string(),
                description: String::new(),
                params: vec![Field {
                    name: "id".to_string(),
                    ty: Type::Str,
                }],
                result: Type::Record {
                    fields: vec![Field {
                        name: "ok".to_string(),
                        ty: Type::Bool,
                    }],
                },
            },
        ],
        ui: vec![
            Screen {
                id: "map".to_string(),
                title: "Map".to_string(),
                layout: LayoutNode::VStack {
                    children: vec![
                        LayoutNode::Button {
                            label: "Reload".to_string(),
                            action: "load".to_string(),
                        },
                        LayoutNode::Button {
                            label: "Add layer".to_string(),
                            action: "add".to_string(),
                        },
                    ],
                },
                actions: vec![
                    UiAction {
                        id: "load".to_string(),
                        description: String::new(),
                        bridge: "map/listLayers".to_string(),
                    },
                    UiAction {
                        id: "add".to_string(),
                        description: String::new(),
                        bridge: "map/addLayer".to_string(),
                    },
                ],
                bindings: vec![UiBinding {
                    field: "layers".to_string(),
                    method: "map/listLayers".to_string(),
                }],
                navigation: vec![UiNavigation {
                    action_id: "add".to_string(),
                    to: "inspector".to_string(),
                }],
            },
            Screen {
                id: "inspector".to_string(),
                title: "Inspector".to_string(),
                actions: vec![],
                ..Screen::default()
            },
        ],
    }
}

/// The schema guide shared by every prompt — written for the model, not for
/// Rust. Describes the exact JSON the validator accepts.
fn schema_guide() -> String {
    let example = serde_json::to_string_pretty(&example_gis_spec())
        .unwrap_or_else(|_| "{\"app\":{}}".to_string());
    // Plain raw string (no format braces) — the GIS example is appended below.
    let guide = r#"APPSPEC JSON SCHEMA (emit exactly this shape):
- app: {"name": "<project name, FIXED — never change it>", "goal": "<one-line goal>"}
- graph: the app's memory-graph schema (what its actors persist/query). Optional
  but strongly encouraged — describe at least the core nodes:
    nodes: [{"name":"<type discriminator>","description":"...","fields":[{"name":"...","ty":<ty>}]}]
    edges: [{"name":"<predicate>","from":"<node type>","to":"<node type>","description":"..."}]
- types: named data types both sides share. Each is tagged by "kind":
    record: {"kind":"record","name":"...","fields":[{"name":"...","ty":<ty>}]}
    enum:   {"kind":"enum","name":"...","variants":["..."]}
- ty (used by every field/param/result): pick exactly one of:
    {"kind":"str"} | {"kind":"int"} | {"kind":"float"} | {"kind":"bool"}
    {"kind":"list","of":<ty>} | {"kind":"option","of":<ty>}
    {"kind":"named","name":"<a type defined in types>"}   (no other "named" uses)
    inline object: {"kind":"record","fields":[{"name":"...","ty":<ty>}]}
- actors: one entry per backend actor. "handlers" = the bridge methods it
  implements (routing is derived from this — no separate route list).
- bridge: the JSON method contract. Each method: name, params, result type.
- ui: SwiftUI screens WITH a layout sketch and interactions. Each screen has:
    layout: a structural tree tagged by "kind":
      {"kind":"vstack","children":[...]} | {"kind":"hstack","children":[...]}
      {"kind":"list","item":<layout>} | {"kind":"text","text":"..."}
      {"kind":"button","label":"...","action":"<action id>"}
      {"kind":"input","placeholder":"...","bind":"<field>"} | {"kind":"spacer"}
    actions: [{"id":"...","description":"...","bridge":"<bridge method>"}]
    bindings: [{"field":"...","method":"<bridge method>"}]
    navigation: [{"action_id":"<an action on this screen>","to":"<a screen id>"}]

HARD RULES (the spec is machine-validated; violations are rejected):
- Every bridge method is handled by exactly ONE actor.
- Every UI action/binding references an existing bridge method.
- Layout buttons reference actions on the SAME screen; navigation targets and
  action ids must exist.
- Graph edge "from"/"to" must reference node types defined in graph.nodes.
- Every "named" ty references a type defined in types.
- There is NO json escape hatch — describe every value precisely.
- Names are unique across types, actors, bridge methods, screens, actions,
  graph node types and graph edge types.
- Keep it small and coherent for the goal (typically 1-3 actors).
- ui layout is a SKETCH: include the screens the goal obviously needs (e.g. a
  main view, an editor/inspector) — do not over-decompose."#;
    format!("{guide}\n\nREFERENCE TEMPLATE (valid GIS example):\n{example}")
}

/// Build the initial requirements prompt for the LLM.
pub fn build_app_spec_prompt(project_name: &str, goal: &str, framework_hints: &str) -> String {
    format!(
        r#"You are the Spire requirements engineer. Given a product goal, derive the formal
AppSpec for a Spire app: a SwiftUI app + Rust actor core connected by a JSON bridge.
The bridge is the single source of truth — UI actions and actor handlers are
projections of the same bridge methods, so they must line up exactly.

{schema}

Spire framework API surface (backend subsystems available to actors via `uses`):
{framework_hints}

Project:
- name: {project_name}   (app.name MUST be exactly "{project_name}")
- goal: {goal}

Reply with ONLY the JSON object. No markdown fencing, no commentary.
"#,
        schema = schema_guide(),
    )
}

/// Build a corrective prompt for the self-healing loop: the original request,
/// plus the concrete validation violations from the previous attempt.
pub fn build_repair_prompt(
    project_name: &str,
    goal: &str,
    framework_hints: &str,
    previous_raw: &str,
    issues: &[SpecIssue],
) -> String {
    let violations = if issues.is_empty() {
        "Your previous reply was not parseable as an AppSpec JSON object.".to_string()
    } else {
        issues
            .iter()
            .map(|i| format!("- [{}] {} — {}", spec_severity_label(i), i.path, i.message))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"{base}

Your previous reply failed validation with these violations:
{violations}

Fix EVERY violation and reply with the COMPLETE corrected AppSpec JSON object only
(no markdown fencing, no commentary, no partial diffs — the full object). The
previous reply is reproduced below for reference; replace it entirely.

Previous reply:
{previous}
"#,
        base = build_app_spec_prompt(project_name, goal, framework_hints),
        violations = violations,
        previous = truncate(previous_raw, 4000),
    )
}

/// Build the improvement-pass prompt: ask the model to critique and refine an
/// already-VALID AppSpec against the goal, without breaking validation.
pub fn build_improve_prompt(
    project_name: &str,
    goal: &str,
    framework_hints: &str,
    spec: &AppSpec,
    warnings: &[SpecIssue],
) -> String {
    let current = serde_json::to_string_pretty(spec).unwrap_or_else(|_| "{}".to_string());
    let warning_lines = if warnings.is_empty() {
        "none".to_string()
    } else {
        warnings
            .iter()
            .map(|i| format!("- [{}] {} — {}", spec_severity_label(i), i.path, i.message))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"{base}

IMPROVEMENT PASS — the AppSpec below is VALID (it passes every machine check).
Critique it against the goal and return an improved, COMPLETE AppSpec JSON
object (no markdown fencing, no commentary, no partial diffs). Improve it only
where it is genuinely thin:
- graph: does it sketch the real persistent state the app's actors will store
  and query? (Spire's graph is the single source of truth.)
- ui: does every screen sketch its layout, bindings and navigation, so the
  screens and their interactions are concrete?
- bridge/actors/types: are the method contract and the domain model coherent
  and complete enough to implement the goal WITHOUT inventing APIs?
Do NOT add features outside the goal, and NEVER break validation.

Warnings from the last pass (non-blocking — resolve them if easy):
{warnings}

Current AppSpec:
{current}
"#,
        base = build_app_spec_prompt(project_name, goal, framework_hints),
        warnings = warning_lines,
        current = current,
    )
}

/// Parse an AppSpec from a raw LLM response. Tolerates markdown code fences
/// and stray prose before/after the JSON object.
pub fn parse_app_spec(raw: &str) -> Option<AppSpec> {
    let t = raw.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    // Fall back to the outermost `{...}` span if prose wrapped the object.
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    let span = &t[start..=end];
    serde_json::from_str(span).ok()
}

/// Emit phase: prompt the LLM for an AppSpec and self-heal against
/// [`validate`] (repair prompts carry the concrete violations) until it
/// validates or [`MAX_ATTEMPTS`] rounds are exhausted.
async fn emit_valid_spec<F, Fut>(
    project_name: &str,
    goal: &str,
    framework_hints: &str,
    call_llm: &mut F,
) -> Result<AppSpec, SpecGenError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let mut last_raw = String::new();
    let mut last_spec: Option<AppSpec> = None;
    let mut last_issues: Vec<SpecIssue> = Vec::new();

    let mut prompt = build_app_spec_prompt(project_name, goal, framework_hints);
    for _attempt in 1..=MAX_ATTEMPTS {
        let raw = call_llm(prompt)
            .await
            .map_err(SpecGenError::LlmUnavailable)?;
        last_raw = raw.clone();
        match parse_app_spec(&raw) {
            Some(spec) => {
                let issues = validate_with_name(&spec, project_name);
                let errors: Vec<SpecIssue> = issues
                    .iter()
                    .filter(|i| i.severity == SpecIssueSeverity::Error)
                    .cloned()
                    .collect();
                if errors.is_empty() {
                    return Ok(spec);
                }
                last_spec = Some(spec);
                last_issues = errors;
                prompt =
                    build_repair_prompt(project_name, goal, framework_hints, &raw, &last_issues);
            }
            None => {
                prompt = build_repair_prompt(project_name, goal, framework_hints, &raw, &[]);
            }
        }
    }

    match last_spec {
        Some(spec) => Err(SpecGenError::Invalid {
            attempts: MAX_ATTEMPTS,
            spec,
            issues: last_issues,
        }),
        None => Err(SpecGenError::Unparseable {
            attempts: MAX_ATTEMPTS,
            last_raw,
        }),
    }
}

/// Run the AppSpec requirements pass to completion:
///
/// 1. **Emit + self-heal** ([`emit_valid_spec`]) — a VALID spec is required;
///    errors keep the pass from returning at all.
/// 2. **Improvement loop** — up to [`MAX_IMPROVE_ROUNDS`] critique + rewrite
///    rounds against the goal (graph schema + UI interactions + domain model
///    completeness). Best-effort: an unreachable LLM, unparseable reply,
///    invalid rewrite, or a rewrite that shrinks the core surface area keeps
///    the current (already valid) spec instead of failing the whole pass.
///
/// `call_llm` is invoked once per round with the full prompt and returns the
/// model's raw reply (or an error string). Injecting it as a closure keeps the
/// loop testable without a live LLM actor.
pub async fn generate_app_spec<F, Fut>(
    project_name: &str,
    goal: &str,
    framework_hints: &str,
    mut call_llm: F,
) -> Result<AppSpec, SpecGenError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    // Phase 1 — emit + self-heal to a valid spec.
    let mut spec = emit_valid_spec(project_name, goal, framework_hints, &mut call_llm).await?;

    // Phase 2 — bounded improvement over the valid spec (never destructive).
    for round in 1..=MAX_IMPROVE_ROUNDS {
        let warnings: Vec<SpecIssue> = validate_with_name(&spec, project_name)
            .into_iter()
            .filter(|i| i.severity == SpecIssueSeverity::Warning)
            .collect();
        let prompt = build_improve_prompt(project_name, goal, framework_hints, &spec, &warnings);
        // Best-effort: any LLM failure here keeps the current valid spec.
        let Ok(raw) = call_llm(prompt).await else {
            break;
        };
        let Some(candidate) = parse_app_spec(&raw) else {
            break;
        };
        let candidate_has_errors = validate_with_name(&candidate, project_name)
            .iter()
            .any(|i| i.severity == SpecIssueSeverity::Error);
        // Regression (invalid or shrunk surface) → keep the current spec.
        if candidate_has_errors || !is_substantive(&candidate, &spec) {
            break;
        }
        // No change → nothing more to gain.
        let json_cur = serde_json::to_string(&spec).unwrap_or_default();
        let json_new = serde_json::to_string(&candidate).unwrap_or_default();
        if json_new == json_cur {
            break;
        }
        tracing::info!(
            "[SpecGen] improvement round {round}/{} applied (valid rewrite)",
            MAX_IMPROVE_ROUNDS
        );
        spec = candidate;
    }

    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOAL: &str = "view and edit map layers";
    const HINTS: &str = "spire-actor + spire-core";

    fn gis_json() -> String {
        serde_json::to_string(&example_gis_spec()).unwrap()
    }

    /// A fake LLM closure that returns each canned reply in order.
    fn canned(
        replies: Vec<String>,
    ) -> impl FnMut(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>>>>
    {
        let mut replies = replies.into_iter();
        move |_prompt: String| {
            let reply = replies.next().unwrap_or_else(|| {
                "{\"app\":{\"name\":\"spire-gis\",\"goal\":\"\"},\"types\":[],\"actors\":[],\"bridge\":[],\"ui\":[]}"
                    .to_string()
            });
            Box::pin(async move { Ok(reply) })
        }
    }

    #[test]
    fn prompt_embeds_project_name_goal_and_framework_hints() {
        let p = build_app_spec_prompt("spire-gis", GOAL, HINTS);
        assert!(
            p.contains("spire-gis"),
            "must fix app.name to the project name"
        );
        assert!(p.contains(GOAL));
        assert!(p.contains(HINTS));
        assert!(p.contains("\"kind\":\"record\""));
        assert!(p.contains("No markdown fencing"));
    }

    #[test]
    fn parse_app_spec_strips_markdown_fences_and_prose() {
        let raw = format!(
            "Sure! Here is the spec:\n```json\n{}\n```\nHope this helps!",
            gis_json()
        );
        let spec = parse_app_spec(&raw).expect("parse must tolerate fences + prose");
        assert_eq!(spec.app.name, "spire-gis");
        assert_eq!(spec.bridge.len(), 2);
    }

    #[test]
    fn valid_spec_is_returned_unmodified() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            canned(vec![gis_json()]),
        ));
        let spec = result.expect("valid spec must pass");
        assert_eq!(spec, example_gis_spec());
        assert!(spec.is_valid());
    }

    #[test]
    fn wrong_app_name_is_repaired() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut wrong = example_gis_spec();
        wrong.app.name = "spire-wrong".to_string();
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            canned(vec![serde_json::to_string(&wrong).unwrap(), gis_json()]),
        ));
        let spec = result.expect("wrong name must be self-healed");
        assert_eq!(spec.app.name, "spire-gis");
    }

    #[test]
    fn dangling_ui_action_is_repaired_across_attempts() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut broken = example_gis_spec();
        broken.ui[0].actions.push(UiAction {
            id: "boom".to_string(),
            description: String::new(),
            bridge: "map/doesNotExist".to_string(),
        });
        let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let prompts2 = prompts.clone();
        let mut replies = vec![serde_json::to_string(&broken).unwrap(), gis_json()];
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            move |prompt: String| {
                prompts2.lock().unwrap().push(prompt.clone());
                // Improvement rounds repeat the (now valid) GIS spec.
                let reply = if replies.is_empty() {
                    gis_json()
                } else {
                    replies.remove(0)
                };
                Box::pin(async move { Ok(reply) })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>>>>
            },
        ));
        let spec = result.expect("repair round must fix the dangling action");
        assert_eq!(spec.ui[0].actions.len(), 2);
        // The REPAIR prompt (index 1 — initial + repair before improvement)
        // must have surfaced the exact violation.
        let prompts = prompts.lock().unwrap();
        let repair = prompts.get(1).expect("second prompt is the repair round");
        assert!(repair.contains("undefined bridge method"));
        assert!(repair.contains("map/doesNotExist"));
        assert!(prompts.last().unwrap().contains("IMPROVEMENT PASS"));
    }

    #[test]
    fn irrecoverable_spec_returns_invalid_with_issues() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut broken = example_gis_spec();
        broken.ui[0].actions.push(UiAction {
            id: "boom".to_string(),
            description: String::new(),
            bridge: "map/doesNotExist".to_string(),
        });
        let broken_json = serde_json::to_string(&broken).unwrap();
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = call_count.clone();
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            move |_prompt: String| {
                let reply = broken_json.clone();
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move { Ok(reply) })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>>>>
            },
        ));
        match result {
            Err(SpecGenError::Invalid {
                attempts,
                issues,
                spec,
            }) => {
                assert_eq!(attempts, MAX_ATTEMPTS);
                assert_eq!(
                    call_count.load(std::sync::atomic::Ordering::SeqCst),
                    MAX_ATTEMPTS
                );
                assert_eq!(spec.app.name, "spire-gis");
                assert!(
                    issues.iter().any(|i| i.severity == SpecIssueSeverity::Error
                        && i.message.contains("undefined bridge method")),
                    "issues: {issues:?}"
                );
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_responses_return_unparseable_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = call_count.clone();
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            move |_prompt: String| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move { Ok("I'm sorry, I cannot do that.".to_string()) })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>>>>
            },
        ));
        match result {
            Err(SpecGenError::Unparseable { attempts, .. }) => {
                assert_eq!(attempts, MAX_ATTEMPTS);
                assert_eq!(
                    call_count.load(std::sync::atomic::Ordering::SeqCst),
                    MAX_ATTEMPTS
                );
            }
            other => panic!("expected Unparseable, got {other:?}"),
        }
    }

    #[test]
    fn llm_failure_maps_to_unavailable_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            move |_prompt: String| {
                Box::pin(async move { Err("actor gone".to_string()) })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>>>>
            },
        ));
        match result {
            Err(SpecGenError::LlmUnavailable(msg)) => assert!(msg.contains("actor gone")),
            other => panic!("expected LlmUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn repair_prompt_lists_issues_and_keeps_schema() {
        let mut broken = example_gis_spec();
        broken.bridge.push(BridgeMethod {
            method: "map/removeLayer".to_string(),
            description: String::new(),
            params: vec![],
            result: Type::Bool,
        });
        let issues: Vec<SpecIssue> = validate(&broken)
            .into_iter()
            .filter(|i| i.severity == SpecIssueSeverity::Error)
            .collect();
        assert!(!issues.is_empty());
        let repair = build_repair_prompt("spire-gis", GOAL, HINTS, &gis_json(), &issues);
        assert!(repair.contains("handled by no actor"));
        assert!(repair.contains("APPSPEC JSON SCHEMA"));
        assert!(repair.contains("COMPLETE corrected AppSpec JSON"));
    }
    #[test]
    fn improvement_loop_applies_a_valid_rewrite_and_is_bounded() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // v2 adds a screen binding; v3 adds a graph node. Each round consumes
        // one reply: emit(gis) → improve(v2, applied) → improve(v3, applied)
        // → stop (budget exhausted after MAX_IMPROVE_ROUNDS).
        let mut v2 = example_gis_spec();
        v2.ui[0].bindings.push(UiBinding {
            field: "active".to_string(),
            method: "map/listLayers".to_string(),
        });
        let mut v3 = v2.clone();
        v3.graph.nodes.push(GraphNodeType {
            name: "viewport".to_string(),
            description: String::new(),
            fields: vec![Field {
                name: "west".to_string(),
                ty: Type::Int,
            }],
        });
        assert!(v3.is_valid());

        let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let prompts2 = prompts.clone();
        let mut replies = vec![
            gis_json(),
            serde_json::to_string(&v2).unwrap(),
            serde_json::to_string(&v3).unwrap(),
        ];
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            move |prompt: String| {
                prompts2.lock().unwrap().push(prompt.clone());
                let reply = if replies.is_empty() {
                    gis_json()
                } else {
                    replies.remove(0)
                };
                Box::pin(async move { Ok(reply) })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>>>>
            },
        ));
        let spec = result.expect("requirements pass must succeed");
        // Emit round + MAX_IMPROVE_ROUNDS improvement rounds.
        assert_eq!(prompts.lock().unwrap().len(), 1 + MAX_IMPROVE_ROUNDS);
        // v3 was applied: graph gained the viewport node.
        assert_eq!(spec.graph.nodes.len(), 2);
        assert!(spec.graph.nodes.iter().any(|n| n.name == "viewport"));
        assert!(spec.is_valid());
        // The improvement prompt embedded the current spec JSON.
        let prompts = prompts.lock().unwrap();
        let improve = prompts[1].clone();
        assert!(improve.contains("IMPROVEMENT PASS"));
        assert!(improve.contains("\"viewport\"") == false); // round 1 saw v1 (no viewport yet)
    }

    #[test]
    fn improvement_loop_rejects_a_shrinking_rewrite() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // The "improved" reply is valid but strips all bridge methods — it
        // must NOT replace the substantive GIS spec.
        let empty = serde_json::json!({
            "app": { "name": "spire-gis", "goal": GOAL },
            "types": [], "actors": [], "bridge": [], "ui": []
        });
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            canned(vec![gis_json(), empty.to_string()]),
        ));
        let spec = result.expect("requirements pass must succeed");
        assert_eq!(spec.bridge.len(), 2, "shrinking rewrite must be rejected");
        assert_eq!(spec, example_gis_spec());
    }

    #[test]
    fn improvement_loop_keeps_valid_spec_when_rewrite_regresses_to_invalid() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut invalid = example_gis_spec();
        invalid.ui[0].actions.push(UiAction {
            id: "boom".to_string(),
            description: String::new(),
            bridge: "map/doesNotExist".to_string(),
        });
        let result = rt.block_on(generate_app_spec(
            "spire-gis",
            GOAL,
            HINTS,
            canned(vec![gis_json(), serde_json::to_string(&invalid).unwrap()]),
        ));
        let spec = result.expect("requirements pass must succeed");
        assert_eq!(spec, example_gis_spec(), "invalid rewrite must be rejected");
    }
}
