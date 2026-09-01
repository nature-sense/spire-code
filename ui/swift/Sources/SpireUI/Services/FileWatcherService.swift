import Foundation

/// Domain service for the file-change event stream. Async push (no polling):
/// each iteration blocks on the FFI `waitForEvent` until the Rust file-watcher
/// signals a change, then yields it.
actor FileWatcherService {
    let backend: any UIBackend

    init(backend: any UIBackend) {
        self.backend = backend
    }

    /// Push-only stream of file-change events. The FFI call blocks until the
    /// next event arrives, so the Rust actor drives the UI.
    func eventStream(timeoutMs: UInt32 = 10000) -> AsyncStream<SpireBridge.FileChangeEvent> {
        AsyncStream { continuation in
            Task.detached {
                while !Task.isCancelled {
                    guard let json = self.backend.waitForEvent(timeoutMs: timeoutMs) else { continue }
                    guard let data = json.data(using: .utf8),
                          let event = try? JSONDecoder().decode(SpireBridge.FileChangeEvent.self, from: data)
                    else { continue }
                    continuation.yield(event)
                }
                continuation.finish()
            }
        }
    }
}