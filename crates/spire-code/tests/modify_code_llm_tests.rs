// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! `modify/code` against a real LLM path — with the *model* replaced, not the code.
//!
//! The flow's unit tests script a backend, so the measured seam and the measure share the
//! author's assumptions. These tests instead point the **real** `LlmActor` at a fake
//! OpenAI-compatible endpoint: the HTTP request, the response parse, the fenced-code
//! stripping, the tree-sitter check and its retry, `select_files`, the apply, the verify —
//! all real. Only the model's *text* is scripted, which is the one thing you want to
//! control anyway.
//!
//! What this cannot tell you is whether a real model gives *good* answers. That is a
//! quality question and needs a live endpoint; this is the plumbing, where the bugs are.

use spire_actor::ActorSystem;
use spire_code::actors::{
    ChatActor, CoordinatorActor, CoordinatorMessage, LlmActor, LlmConfig, McpClientActor,
    SystemActor, ToolsActor,
};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use tokio::sync::mpsc;

/// A sender that accepts everything and answers nothing.
fn mock_sender<T: Send + 'static>() -> mpsc::Sender<T> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

/// The coordinator's memory-graph channel with no actor behind it: every read comes back
/// empty, which is what makes "the build reported no errors" the baseline here.
fn mock_memory_graph() -> mpsc::Sender<MemoryGraphMessage> {
    let (tx, _rx) = mpsc::channel(64);
    tx
}

/// A fake OpenAI-compatible endpoint, serving one scripted reply per request.
///
/// Blocking std IO on its own thread on purpose: the test runtime is busy driving the
/// actors, and this needs nothing beyond the standard library.
fn fake_llm(replies: Vec<String>) -> String {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut queue: std::collections::VecDeque<String> = replies.into();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            // Read the headers, then however much body the Content-Length promises.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut wanted: Option<usize> = None;
            loop {
                let Ok(n) = stream.read(&mut chunk) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let length = *wanted.get_or_insert_with(|| {
                        String::from_utf8_lossy(&buf[..head_end])
                            .to_lowercase()
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0)
                    });
                    if buf.len() >= head_end + 4 + length {
                        break;
                    }
                }
            }

            let content = queue.pop_front().unwrap_or_else(|| "NONE".to_string());
            let body = serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": content },
                    "finish_reason": "stop"
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/v1/chat/completions")
}

/// A coordinator wired to a real LLM actor pointed at `url`.
///
/// Everything else is a mock: the build and test tools answer nothing, so a run's
/// baseline is "nothing known to be broken" and the verdict is decided by the code under
/// test rather than by a stub toolchain.
async fn app(url: &str) -> mpsc::Sender<CoordinatorMessage> {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig {
        api_url: url.to_string(),
        // The actor refuses a request with no key, but never checks it against anything:
        // our endpoint does not care what it is.
        api_key: "test-key".to_string(),
        ..LlmConfig::default()
    }));
    let (system_tx, _) = system.spawn(SystemActor::new());

    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        system_tx,
        mock_memory_graph(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
    ));
    coord_tx
}

/// Send one request and wait for the coordinator's answer.
async fn call(
    coord: &mpsc::Sender<CoordinatorMessage>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let (tx, rx) = tokio::sync::oneshot::channel();
    coord
        .send(CoordinatorMessage::HandleRequest {
            method: method.to_string(),
            params,
            response_tx: tx,
        })
        .await
        .unwrap();
    rx.await.unwrap()
}

/// A project with one source file and a build directory, so a platform resolves.
fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.cpp");
    std::fs::write(&file, "int value = 1;\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("build-fake")).unwrap();
    (tmp, file)
}

/// What `modify/code` is asked to do, shaped as the UI sends it.
fn request(root: &std::path::Path, scope: &[&std::path::Path]) -> serde_json::Value {
    serde_json::json!({
        "path": root.to_string_lossy(),
        "prompt": "make value 2",
        "platform": "fake",
        "scope": scope
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
    })
}

/// The whole path with a model behind it: the listing call picks the file, the rewrite
/// call returns fenced C++, and the change lands and is kept.
#[tokio::test]
async fn rewrites_the_file_the_model_names() {
    let (tmp, file) = project();
    let url = fake_llm(vec![
        file.to_string_lossy().to_string(),
        "```cpp\nint value = 2;\n```\n".to_string(),
    ]);
    let coord = app(&url).await;

    let result = call(&coord, "modify/code", request(tmp.path(), &[&file])).await;

    assert_eq!(result["success"], serde_json::json!(true), "{result}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "int value = 2;",
        "the fenced block was unwrapped and written: {result}"
    );
}

/// Models decorate. The path list is read through prose, bullets and backticks — this is
/// the parsing step that would otherwise write to a file nobody named.
#[tokio::test]
async fn reads_a_path_list_through_prose_and_bullets() {
    let (tmp, file) = project();
    let url = fake_llm(vec![
        format!(
            "Here is the file that needs changing:\n\n- `{}`\n",
            file.display()
        ),
        "```cpp\nint value = 3;\n```".to_string(),
    ]);
    let coord = app(&url).await;

    let result = call(&coord, "modify/code", request(tmp.path(), &[&file])).await;

    assert_eq!(result["success"], serde_json::json!(true), "{result}");
    // No trailing newline: unwrapping the fence trims it, and the write is verbatim.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "int value = 3;");
}

/// A model naming a file that was never offered must get nothing. The scope is a
/// guardrail, not a hint.
#[tokio::test]
async fn will_not_touch_a_file_outside_the_scope() {
    let (tmp, file) = project();
    let outside = tmp.path().join("b.cpp");
    std::fs::write(&outside, "int untouched = 7;\n").unwrap();

    let url = fake_llm(vec![outside.to_string_lossy().to_string()]);
    let coord = app(&url).await;

    let result = call(&coord, "modify/code", request(tmp.path(), &[&file])).await;

    assert_eq!(result["success"], serde_json::json!(false), "{result}");
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "int untouched = 7;\n",
        "the file it named was not offered, so it was not touched: {result}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "int value = 1;\n");
}

/// A rewrite that does not parse is retried once (feeding the syntax errors back), and if
/// it still does not parse it is dropped. Nothing garbled is written, and the run says so
/// rather than reporting a success with no changes.
#[tokio::test]
async fn drops_a_rewrite_that_does_not_parse() {
    let (tmp, file) = project();
    let url = fake_llm(vec![
        file.to_string_lossy().to_string(),
        "int broken( ;\n".to_string(),
        // The retry's answer, just as broken.
        "int still_broken( ;\n".to_string(),
    ]);
    let coord = app(&url).await;

    let result = call(&coord, "modify/code", request(tmp.path(), &[&file])).await;

    assert_eq!(result["success"], serde_json::json!(false), "{result}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "int value = 1;\n",
        "the unparseable rewrite was never written: {result}"
    );
}
