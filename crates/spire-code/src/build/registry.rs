// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **ESP Component Registry** as a tool: `registry/search` and `registry/component`.
//!
//! The design phase can only say that a component *exists* if something can tell it so. A library on
//! this machine is read off disk (`idf_projects::library_facts`), but the display driver and the touch
//! controller the next applications need are **published** components —
//! `espressif/esp_lcd_st7701`, `espressif/esp_lcd_touch_gt911` — and the honest way to name one is to
//! look it up rather than to design a second copy of it.
//!
//! Nothing here is cached and nothing is vendored, and that is the difference between this and a corpus:
//! a corpus is *documentation*, read once and embedded, while a component's version, its targets and its
//! dependencies are facts that change. A design that names a version older than the registry's is a build
//! that fails on the machine that has the newer one, and no amount of retrieval can prevent that.
//!
//! The reply is trimmed to what a design — or a person reading a design — acts on: the name, what it is,
//! which chips it is for, its licence, and what it pulls in. The registry's own entry is a version's whole
//! build metadata, and the parts nobody reads are the parts that make a tool result unreadable.

use serde_json::{json, Value};
use spire_core::actors::ToolInfo;
use std::time::Duration;

const REGISTRY_API: &str = "https://components.espressif.com/api/components";
const USER_AGENT: &str = "spire/0.1 (NatureSense AI-Traps; opensource)";

/// How many versions of a component are described. A design picks the newest compatible one; a caller
/// that needs an old one can name it. The registry returns *every* version, and passing thirty of them
/// through a tool result is a wall of text nobody asked for.
const VERSIONS_SHOWN: usize = 5;

/// How many search hits are described when the caller does not say.
const DEFAULT_LIMIT: usize = 10;

/// Static tool definitions surfaced to the LLM.
pub fn tool_definitions() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "registry/search".to_string(),
            description: "Search the ESP Component Registry (components.espressif.com) for published \
                          components by name or keyword. Use it before designing a driver: if a \
                          component for the device already exists, the design names it instead of \
                          writing a second copy."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Name or keyword, e.g. `st7701`" },
                    "limit": { "type": "integer", "description": "Max components (default 10)" }
                },
                "required": ["query"]
            }),
        },
        ToolInfo {
            name: "registry/component".to_string(),
            description: "One published component, by name: its newest versions, the chips it targets, \
                          its licence and the components it depends on. `namespace` is only needed when \
                          the name is published by more than one publisher, and the refusal says so."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Component name, e.g. `esp_lcd_st7701`" },
                    "namespace": { "type": "string", "description": "Publisher, when ambiguous" },
                    "versions": { "type": "integer", "description": "How many versions to describe (default 5)" }
                },
                "required": ["name"]
            }),
        },
    ]
}

/// Route a tool call to the registry.
pub async fn call(tool_name: &str, args: Value) -> Result<Value, String> {
    match tool_name {
        "registry/search" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or("registry/search: 'query' is required")?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|limit| limit as usize)
                .unwrap_or(DEFAULT_LIMIT);
            search(query, limit).await
        }
        "registry/component" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or("registry/component: 'name' is required")?;
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|namespace| !namespace.is_empty());
            let versions = args
                .get("versions")
                .and_then(Value::as_u64)
                .map(|versions| versions as usize)
                .unwrap_or(VERSIONS_SHOWN);
            component(namespace, name, versions).await
        }
        other => Err(format!("registry: unknown tool '{other}'")),
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// Search, and describe each hit the way a design reads it.
pub async fn search(query: &str, limit: usize) -> Result<Value, String> {
    let entries = get_array(REGISTRY_API, &[("q", query)]).await?;
    let components: Vec<Value> = entries.iter().filter_map(summarize).take(limit).collect();
    let more = entries.len() > components.len();
    Ok(json!({
        "query": query,
        "count": components.len(),
        "total": entries.len(),
        "components": components,
        "note": if more {
            "more components matched than are shown — raise `limit`, or name the one you mean"
        } else {
            ""
        },
    }))
}

/// One component, by name — and by publisher where a name alone is not enough.
///
/// A name published by two people (`esp_lcd_st7701` is Espressif's, and Nicolaielectronics' as well) has
/// no single answer, so an unnamed publisher is resolved by asking the registry which names match
/// *exactly* and refusing when more than one does. The refusal lists them, because the caller's next move
/// is to say which — and a design that named the wrong publisher's driver would be a design built on
/// somebody else's code.
pub async fn component(
    namespace: Option<&str>,
    name: &str,
    versions: usize,
) -> Result<Value, String> {
    let (namespace, entry) = match namespace {
        Some(namespace) => {
            let entry = get_json(&format!("{REGISTRY_API}/{namespace}/{name}"), &[]).await?;
            (namespace.to_string(), entry)
        }
        None => {
            let hits = get_array(REGISTRY_API, &[("q", name)]).await?;
            resolve(&hits, name)?
        }
    };

    let all = entry
        .get("versions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let described: Vec<Value> = all.iter().take(versions).map(version).collect();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(name)
        .to_string();
    Ok(json!({
        "name": name,
        "namespace": namespace,
        // The fully-qualified form is what an `idf_component.yml` dependency is written with, and it is
        // the whole point of looking a component up rather than guessing at it.
        "component": format!("{namespace}/{name}"),
        "versions_shown": described.len(),
        "versions_total": all.len(),
        "versions": described,
    }))
}

/// `GET` a registry endpoint and parse its JSON, with the registry's own answer in the error when it
/// refuses.
async fn get_json(url: &str, query: &[(&str, &str)]) -> Result<Value, String> {
    let reply = client()?
        .get(url)
        .query(query)
        .send()
        .await
        .map_err(|e| format!("the registry could not be reached: {e}"))?;
    let status = reply.status();
    if !status.is_success() {
        return Err(format!(
            "the registry answered {status} for {}",
            reply.url()
        ));
    }
    reply
        .json()
        .await
        .map_err(|e| format!("the registry's reply did not parse: {e}"))
}

/// Which component a bare name means, from a search's hits — or why it cannot be told.
///
/// A **pure** function, and separated for that reason: the two refusals are the interesting behaviour and
/// they are the part a network cannot exercise deterministically. `name` must match **exactly** — a
/// search for `st7701` returns components whose *descriptions* mention it, and naming one of those would
/// be naming a component nobody asked for.
fn resolve(hits: &[Value], name: &str) -> Result<(String, Value), String> {
    let exact: Vec<(String, Value)> = hits
        .iter()
        .filter(|hit| hit.get("name").and_then(Value::as_str) == Some(name))
        .filter_map(|hit| {
            hit.get("namespace")
                .and_then(Value::as_str)
                .map(|namespace| (namespace.to_string(), hit.clone()))
        })
        .collect();
    match exact.as_slice() {
        [(namespace, entry)] => Ok((namespace.clone(), entry.clone())),
        [] => {
            let seen: Vec<String> = hits
                .iter()
                .filter_map(|hit| {
                    let name = hit.get("name")?.as_str()?;
                    let namespace = hit.get("namespace")?.as_str()?;
                    Some(format!("{namespace}/{name}"))
                })
                .take(8)
                .collect();
            Err(format!(
                "registry/component: nothing is published as '{name}'. Searching for it found: {}",
                if seen.is_empty() {
                    "nothing".to_string()
                } else {
                    seen.join(", ")
                }
            ))
        }
        _ => {
            let namespaces: Vec<String> = exact
                .iter()
                .map(|(namespace, _)| format!("{namespace}/{name}"))
                .collect();
            Err(format!(
                "registry/component: '{name}' is published by more than one namespace: {}. Say which one \
                 with `namespace`.",
                namespaces.join(", ")
            ))
        }
    }
}

/// `GET` a registry endpoint that promises a **list** (`?q=`), refusing anything else by name. The
/// search endpoint answers with an array and the component endpoint with an object, so the two are
/// separate calls rather than one that returns whatever arrived.
async fn get_array(url: &str, query: &[(&str, &str)]) -> Result<Vec<Value>, String> {
    match get_json(url, query).await? {
        Value::Array(entries) => Ok(entries),
        other => {
            let seen: String = other.to_string().chars().take(200).collect();
            Err(format!(
                "the registry answered with {seen} where a list of components was expected"
            ))
        }
    }
}

/// One search hit, as a design reads it.
///
/// The registry states a component's description, targets, licence and dependencies **on its latest
/// version**, not on the entry itself — which is the trap this function exists to absorb. A hit with no
/// version is skipped rather than described as an empty component: it is published but has nothing to
/// install, and a design that named it would be a design that cannot build.
fn summarize(hit: &Value) -> Option<Value> {
    let name = hit.get("name").and_then(Value::as_str)?;
    let namespace = hit.get("namespace").and_then(Value::as_str)?;
    let latest = hit.get("latest_version")?;
    Some(json!({
        "component": format!("{namespace}/{name}"),
        "name": name,
        "namespace": namespace,
        "version": latest.get("version").and_then(Value::as_str).unwrap_or(""),
        "description": latest.get("description").and_then(Value::as_str).unwrap_or(""),
        "targets": latest.get("targets").cloned().unwrap_or_else(|| json!([])),
        "license": latest
            .get("license")
            .and_then(|license| license.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        "repository": latest.get("repository").and_then(Value::as_str).unwrap_or(""),
        "dependencies": dependencies(latest),
    }))
}

/// One version, as a design reads it: what it is, what it runs on, what it pulls in.
fn version(version: &Value) -> Value {
    json!({
        "version": version.get("version").and_then(Value::as_str).unwrap_or(""),
        "description": version.get("description").and_then(Value::as_str).unwrap_or(""),
        "targets": version.get("targets").cloned().unwrap_or_else(|| json!([])),
        "license": version
            .get("license")
            .and_then(|license| license.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        "dependencies": dependencies(version),
    })
}

/// The registry's dependency entries, narrowed to what a caller can act on.
///
/// `source: "idf"` is the IDF itself: a version range with **no component name** (the registry's way of
/// saying "this needs IDF ≥ 5.4"), which is why a name may be absent here and why the two shapes are kept
/// apart rather than flattened into one that lies about what the dependency is.
fn dependencies(version: &Value) -> Value {
    let list: Vec<Value> = version
        .get("dependencies")
        .and_then(Value::as_array)
        .map(|dependencies| {
            dependencies
                .iter()
                .map(|dependency| {
                    let source = dependency
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or("service");
                    let spec = dependency.get("spec").and_then(Value::as_str).unwrap_or("");
                    match (
                        dependency.get("namespace").and_then(Value::as_str),
                        dependency.get("name").and_then(Value::as_str),
                    ) {
                        (Some(namespace), Some(name)) => json!({
                            "component": format!("{namespace}/{name}"),
                            "spec": spec,
                            "source": source,
                        }),
                        _ => json!({ "idf": spec, "source": source }),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    json!(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry of a real `?q=st7701` search, with the bulky parts removed.
    ///
    /// Taken verbatim from `https://components.espressif.com/api/components/?q=st7701` — the keys are the
    /// API's, and the ones dropped (`checksums`, `docs`, `examples`, `info_metadata_keys`,
    /// `build_metadata_keys`, `maintainers`, `id`, `component_hash`, `created_at`, `discussion`,
    /// `homepage`, `downloads_total`, `documentation`, `url`, `yanked_*`) are the ones this tool does not
    /// read. What is left is the shape a design depends on, including the two facts that are easy to get
    /// wrong: the description, the targets, the licence and the dependencies live on the **version**, and
    /// an `idf` dependency has a **null name** because it is the IDF itself.
    const A_REAL_SEARCH_HIT: &str = r#"{
      "name": "esp_lcd_st7701",
      "namespace": "espressif",
      "latest_version": {
        "version": "2.0.2~2",
        "description": "ESP LCD ST7701(RGB & MIPI-DSI)",
        "dependencies": [
          { "is_public": false, "matches": [], "name": "cmake_utilities", "namespace": "espressif",
            "registry_url": "https://components.espressif.com", "require": true, "rules": [],
            "source": "service", "spec": "0.*" },
          { "is_public": false, "matches": [], "name": null, "namespace": null, "registry_url": null,
            "require": true, "rules": [], "source": "idf", "spec": ">=5.4" }
        ],
        "license": { "name": "Apache-2.0", "url": "https://…/license.txt" },
        "repository": "git://github.com/espressif/esp-iot-solution.git",
        "targets": ["esp32s3", "esp32s31", "esp32p4"]
      }
    }"#;

    /// A search hit, read the way a design reads it: what it is, which chips, what it pulls in — and the
    /// IDF itself kept apart from a component, because a component is what a design can name.
    #[test]
    fn a_search_hit_is_read_from_the_version_it_is_stated_on() {
        let hit: Value = serde_json::from_str(A_REAL_SEARCH_HIT).expect("the payload parses");
        let summary = summarize(&hit).expect("a published component with a version");

        assert_eq!(summary["component"], "espressif/esp_lcd_st7701");
        assert_eq!(summary["version"], "2.0.2~2");
        assert_eq!(summary["description"], "ESP LCD ST7701(RGB & MIPI-DSI)");
        assert_eq!(summary["license"], "Apache-2.0");
        assert_eq!(
            summary["targets"],
            json!(["esp32s3", "esp32s31", "esp32p4"])
        );
        assert_eq!(
            summary["dependencies"],
            json!([
                { "component": "espressif/cmake_utilities", "spec": "0.*", "source": "service" },
                { "idf": ">=5.4", "source": "idf" }
            ]),
            "the IDF is a version range with no component name, and calling it a component would be a \
             lie a design would act on"
        );

        // A component with no version has nothing to install: it is skipped rather than described as an
        // empty one, because a design that named it would be a design that cannot build.
        let versionless = json!({ "name": "ghost", "namespace": "nobody" });
        assert!(summarize(&versionless).is_none());
    }

    /// A bare name resolves to a publisher — or refuses, naming what it did see.
    ///
    /// `esp_lcd_st7701` is published by Espressif *and* by Nicolaielectronics, which is why a name alone is
    /// not always an answer; and a search for `st7701` returns components that merely mention it, which is
    /// why the match has to be exact.
    #[test]
    fn a_bare_component_name_resolves_exactly_or_says_why_not() {
        let espresso = json!({ "name": "esp_lcd_st7701", "namespace": "espressif" });
        let theirs = json!({ "name": "esp_lcd_st7701", "namespace": "nicolaielectronics" });
        let mention = json!({ "name": "esp_lcd_st7701_wrapper", "namespace": "somebody" });

        let (namespace, _) = resolve(std::slice::from_ref(&espresso), "esp_lcd_st7701")
            .expect("one exact match is an answer");
        assert_eq!(namespace, "espressif");

        let ambiguous = resolve(&[espresso, theirs], "esp_lcd_st7701")
            .expect_err("two publishers is not an answer");
        assert!(
            ambiguous.contains("espressif/esp_lcd_st7701")
                && ambiguous.contains("nicolaielectronics/esp_lcd_st7701")
                && ambiguous.contains("Say which one"),
            "{ambiguous}"
        );

        let absent = resolve(&[mention], "esp_lcd_st7701").expect_err("a near miss is not a match");
        assert!(
            absent.contains("nothing is published as 'esp_lcd_st7701'")
                && absent.contains("somebody/esp_lcd_st7701_wrapper"),
            "the refusal lists what the search did find:\n{absent}"
        );

        let nothing =
            resolve(&[], "esp_lcd_st7701").expect_err("nothing at all is still a refusal");
        assert!(nothing.contains("found: nothing"), "{nothing}");
    }

    /// The **live** gate: the endpoint the fixtures were taken from still answers, and still in this shape.
    ///
    /// Ignored by default because it needs the network, and a suite that fails on a plane teaches people
    /// to ignore failures. Everything it checks is also checked offline — what only this can catch is the
    /// registry moving, which is exactly the fact the design phase depends on and the one a fixture
    /// freezes in the past.
    #[test]
    #[ignore = "reaches components.espressif.com"]
    fn the_registry_answers_the_shape_the_design_reads() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");

        let found = runtime
            .block_on(search("esp_lcd_st7701", 5))
            .expect("the registry answers");
        let components = found["components"].as_array().cloned().unwrap_or_default();
        assert!(
            !components.is_empty(),
            "the registry has published esp_lcd_st7701 since 2023:\n{found}"
        );
        let ours = components
            .iter()
            .find(|component| component["component"] == "espressif/esp_lcd_st7701")
            .unwrap_or_else(|| panic!("espressif's own driver is among the hits:\n{found}"));
        assert!(
            ours["targets"]
                .as_array()
                .is_some_and(|targets| !targets.is_empty()),
            "a driver names the chips it is for:\n{ours}"
        );
        assert!(
            !ours["description"].as_str().unwrap_or("").is_empty(),
            "{ours}"
        );

        let one = runtime
            .block_on(component(Some("espressif"), "esp_lcd_st7701", 3))
            .expect("one component by name");
        assert_eq!(one["component"], "espressif/esp_lcd_st7701");
        assert_eq!(
            one["versions"].as_array().map(Vec::len),
            Some(3),
            "the caller asked for three versions:\n{one}"
        );
        assert!(one["versions_total"].as_u64().unwrap_or(0) >= 3, "{one}");

        // And the touch controller the next application needs, looked up by **name alone** — the
        // resolution path a design takes when it knows what the part is called.
        let touch = runtime
            .block_on(component(None, "esp_lcd_touch_gt911", 1))
            .expect("a name alone resolves when one publisher has it");
        assert_eq!(touch["name"], "esp_lcd_touch_gt911");
    }
}
