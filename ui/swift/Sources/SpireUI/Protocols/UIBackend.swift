import Foundation

/// Abstract interface for communication with the Rust core.
///
/// Any UI frontend (SwiftUI, CLI, web) can implement this protocol
/// to communicate with the core via FlatBuffers messages.
protocol UIBackend: Sendable {
    /// Whether the Rust core is reachable (e.g. dylib loaded). Defaults to true
    /// for in-process/mock backends; the FFI backend reports whether it could
    /// `dlopen` libspire_code.dylib.
    var isAvailable: Bool { get }
    /// Send a FlatBuffers-encoded command and await the reply.
    func send(_ data: Data) async throws -> Data
    /// Block until the next pushed file-change event is available (or timeout).
    /// Returns the event JSON string, or nil on timeout.
    func waitForEvent(timeoutMs: UInt32) -> String?

    /// Drain accumulated streaming build events as JSON array string (or nil).
    func drainBuildEvents() -> String?

    /// Block until a build event is pushed (async push — no polling), up to
    /// timeout_ms. Returns drained events as JSON array string, or nil on timeout.
    func waitForBuildEvent(timeoutMs: UInt32) -> String?
}

extension UIBackend {
    var isAvailable: Bool { true }
}