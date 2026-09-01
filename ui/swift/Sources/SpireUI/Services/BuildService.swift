import Foundation
import Observation

/// Domain service owning all build/lint/fix operations plus the single live
/// build-event consumer. Being an actor serializes access to the shared, mutable
/// build-event state, eliminating the race between the event waiter and the
/// view's buildEvents mutation.
///
/// The Rust side uses tokio::sync::Notify (async push): the forwarder task
/// pushes events to a shared buffer and signals BUILD_NOTIFY; the FFI waiter
/// drains first, then waits — so there is exactly one consumer here.
struct BuildToolResult {
    let success: Bool
    let output: String
    let command: String?
    let durationSecs: Double?
    let error: String?
    let buildEvents: [SpireBridge.BuildEventLine]
}

actor BuildService {
    let backend: any UIBackend

    /// Single, long-lived consumer for build events. Notify only wakes ONE
    /// waiter per notify_one(), so there must be exactly one of these.
    private var drainTask: Task<Void, Never>?
    private var onEvents: (@Sendable ([SpireBridge.BuildEventLine]) -> Void)?

    init(backend: any UIBackend) {
        self.backend = backend
    }

    // MARK: - FFI calls

    /// Run a build tool (build/lint/fix/clean/test) and return the result.
    func runTool(_ tool: String, path: String, language: String = "Rust", package: String? = nil, platform: String? = nil, target: String? = nil) async throws -> BuildToolResult {
        var body: [String: Any] = [
            "method": "tools/call",
            "params": [
                "tool": tool,
                "args": [
                    "path": path,
                    "language": language
                ] as [String: Any]
            ]
        ]
        // Target a specific workspace package, e.g. `cargo build --package spire-code`.
        if let package {
            if var params = body["params"] as? [String: Any],
               var args = params["args"] as? [String: Any] {
                args["package"] = package
                params["args"] = args
                body["params"] = params
            }
        }
        // Cross-platform build target (e.g. "host" or "rpi5" for Meson projects).
        if let platform {
            if var params = body["params"] as? [String: Any],
               var args = params["args"] as? [String: Any] {
                args["platform"] = platform
                params["args"] = args
                body["params"] = params
            }
        }
        // Specific build target within the project (e.g. Meson executable name).
        if let target {
            if var params = body["params"] as? [String: Any],
               var args = params["args"] as? [String: Any] {
                args["target"] = target
                params["args"] = args
                body["params"] = params
            }
        }
        let data = try JSONSerialization.data(withJSONObject: body)
        let reply = try await backend.send(data)
        let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any]
        if let err = json?["error"] as? String {
            return BuildToolResult(success: false, output: "", command: nil, durationSecs: nil, error: err, buildEvents: [])
        }
        // Module returns {"result":{"content":[{"type":"text","text":"{...}"}],"isError":false}}
        // where inner text is JSON: {"success":true,"output":"...","command":"...","buildEvents":[...]}
        if let result = json?["result"] as? [String: Any],
           let content = result["content"] as? [[String: Any]],
           let first = content.first,
           let text = first["text"] as? String,
           let payload = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] {
            return BuildToolResult(
                success: payload["success"] as? Bool ?? false,
                output: payload["output"] as? String ?? text,
                command: payload["command"] as? String,
                durationSecs: payload["durationSecs"] as? Double,
                error: payload["error"] as? String,
                buildEvents: parseBuildEvents(payload["buildEvents"])
            )
        }
        if let success = json?["success"] as? Bool {
            return BuildToolResult(
                success: success,
                output: json?["output"] as? String ?? String(data: reply, encoding: .utf8) ?? "",
                command: json?["command"] as? String,
                durationSecs: json?["durationSecs"] as? Double,
                error: json?["error"] as? String,
                buildEvents: parseBuildEvents(json?["buildEvents"])
            )
        }
        throw BuildServiceError.unrecognizedResponse
    }

    /// Fetch Markdown documentation for a dependency package.
    func dependencyDocs(name: String, version: String?, language: String = "Rust") async -> String? {
        do {
            var params: [String: Any] = [
                "tool": "build_dependency_docs",
                "args": [
                    "name": name,
                    "language": language
                ]
            ]
            if let version, !version.isEmpty {
                var args = params["args"] as? [String: Any] ?? [:]
                args["version"] = version
                params["args"] = args
            }
            let body: [String: Any] = [
                "method": "tools/call",
                "params": params
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] {
                if let err = json["error"] as? String { return nil }
                if let result = json["result"] as? [String: Any] {
                    if let content = result["content"] as? [[String: Any]],
                       let first = content.first,
                       let text = first["text"] as? String {
                        if let md = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
                           let markdown = md["markdown"] as? String {
                            return markdown
                        }
                        return text
                    }
                    if let isError = result["isError"] as? Bool, isError { return nil }
                }
            }
            return nil
        } catch {
            return nil
        }
    }

    // MARK: - Live event consumer (async push, no polling)

    /// Start the single build-event consumer. Idempotent — calling again does
    /// not stack a second waiter (which would steal notifications from this one).
    func startEventConsumer(handler: @escaping @Sendable ([SpireBridge.BuildEventLine]) -> Void) {
        guard drainTask == nil else { return }
        self.onEvents = handler
        drainTask = Task.detached { [weak self] in
            while !Task.isCancelled {
                guard let self else { break }
                guard let json = self.backend.waitForBuildEvent(timeoutMs: 10000),
                      let data = json.data(using: .utf8),
                      let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]],
                      !arr.isEmpty
                else { continue }
                let lines = arr.compactMap { BuildService.parse($0) }
                if !lines.isEmpty {
                    // Deliver on the main actor so UI state mutations are serial.
                    await self.deliver(lines)
                }
            }
        }
    }

    /// Stop the single build-event consumer.
    func stopEventConsumer() {
        drainTask?.cancel()
        drainTask = nil
        onEvents = nil
    }

    func deliver(_ lines: [SpireBridge.BuildEventLine]) async {
        let handler = onEvents
        await MainActor.run {
            handler?(lines)
        }
    }

    private static func parse(_ d: [String: Any]) -> SpireBridge.BuildEventLine? {
        guard let line = d["line"] as? String else { return nil }
        return SpireBridge.BuildEventLine(
            line: line,
            level: d["level"] as? String ?? "info",
            target: d["target"] as? String,
            file: d["file"] as? String,
            lineNumber: d["line_number"] as? Int,
            message: d["message"] as? String,
            detail: d["detail"] as? String
        )
    }

    private func parseBuildEvents(_ value: Any?) -> [SpireBridge.BuildEventLine] {
        guard let arr = value as? [[String: Any]] else { return [] }
        return arr.compactMap { Self.parse($0) }
    }
}

enum BuildServiceError: Error {
    case unrecognizedResponse
}