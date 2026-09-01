import Foundation
import Observation

/// Container view model for the build panel. Owns the async build lifecycle and
/// the live event stream, exposing a single `LoadingState` the view switches on.
/// The view stays a pure presenter — no async code in the View struct.
@MainActor
@Observable
final class BuildPanelViewModel {
    /// The domain service. Injected (defaulted) so tests/previews can substitute
    /// a mock backend.
    let service: BuildService

    /// Current build/lint/fix/test lifecycle state.
    var state: LoadingState<BuildToolResult> = .idle

    /// Live build event lines streamed from the Rust side (async push, no polling).
    var liveEvents: [SpireBridge.BuildEventLine] = []

    /// The tool currently running (e.g. "build_build"), nil when idle.
    var runningTool: String?

    init(service: BuildService) {
        self.service = service
    }

    /// Begin the single live-event consumer. Idempotent — BuildService guarantees
    /// exactly one waiter so tokio Notify notifications are never stolen.
    func startEventConsumer() {
        Task { [service] in
            await service.startEventConsumer { lines in
                // BuildService.deliver already runs its handler on the MainActor,
                // but the closure is @Sendable, so hop explicitly to satisfy
                // Swift's actor isolation checker.
                Task { @MainActor [weak self] in
                    self?.liveEvents.append(contentsOf: lines)
                }
            }
        }
    }

    /// Stop the live-event consumer (e.g. on view disappear).
    func stopEventConsumer() {
        Task { [service] in
            await service.stopEventConsumer()
        }
    }

    /// Run a build tool with proper lifecycle state transitions:
    /// idle → loading → success/failure.
    func runTool(_ tool: String, path: String, language: String = "Rust", package: String? = nil, platform: String? = nil, target: String? = nil) async {
        runningTool = tool
        state = .loading
        liveEvents = []

        do {
            let result = try await service.runTool(tool, path: path, language: language, package: package, platform: platform, target: target)
            // The live event stream delivers lines incrementally while the tool runs.
            // The result's buildEvents contains the same lines again (collected by the
            // Rust build manager into the RPC payload) — only use them as a fallback
            // if the live stream delivered nothing (e.g. consumer not started), to
            // avoid showing the complete log twice.
            // Meson lint/analyze puts its diagnostics in `output` (the
            // buildEvents stream may only contain a synthetic "finished"
            // event), so materialize the FULL output as visible build-log
            // lines — otherwise the panel would only show one line.
            // Do this unconditionally so the analyzer's findings always appear
            // (lines that were already streamed are de-duplicated by ID).
            var lines = result.buildEvents
            let streamedIds = Set(lines.map(\.id))
            for line in result.output.split(separator: "\n") {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                if trimmed.isEmpty { continue }
                let ev = SpireBridge.BuildEventLine(
                    line: String(line),
                    level: result.success ? "info" : "error",
                    target: nil
                )
                if streamedIds.contains(ev.id) { continue }
                lines.append(ev)
            }
            liveEvents = lines
            state = .success(result)
        } catch {
            state = .failure(error)
        }
        runningTool = nil
    }

    /// True whenever a build tool is in flight (drives the spinner/header).
    var isRunning: Bool {
        runningTool != nil || state.isLoading
    }
}