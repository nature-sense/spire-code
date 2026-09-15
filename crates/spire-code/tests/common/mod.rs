// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Shared harness for the system-level tests.
//!
//! `#![allow(dead_code)]` because every test binary compiles this module separately and
//! uses a different subset of it.

#![allow(dead_code)]

use tokio::sync::mpsc;

/// A sender that accepts everything and answers nothing.
pub fn mock_sender<T: Send + 'static>() -> mpsc::Sender<T> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

/// A fake OpenAI-compatible endpoint, serving one scripted reply per request.
///
/// This is what makes the model the *only* thing replaced: the real `LlmActor` posts to
/// it over HTTP, parses the reply, and everything downstream is production code. Blocking
/// std IO on its own thread on purpose — the test runtime is busy driving actors.
pub fn fake_llm(replies: Vec<String>) -> String {
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
