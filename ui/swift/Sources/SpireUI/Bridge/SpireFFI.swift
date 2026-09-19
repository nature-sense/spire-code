import Foundation

/// Backend that communicates with the Rust core via JSON C FFI.
/// Locates libspire_code.dylib either inside the .app bundle
/// (Contents/Frameworks) or, during development, relative to the
/// project source tree.
final class SpireFFIBackend: UIBackend, @unchecked Sendable {

    private let loaded: Bool
    private let handle: UnsafeMutableRawPointer?

    /// True when libspire_code.dylib was loaded successfully (Rust core reachable).
    var isAvailable: Bool { loaded }

    init() {
        if let (h, path) = SpireFFIBackend.resolveHandle() {
            print("SpireFFI: loaded \(path)")
            self.handle = h
            self.loaded = true
        } else {
            print("SpireFFI: libspire_code.dylib not found (tried \(SpireFFIBackend.candidatePaths())) — running without Rust core")
            self.handle = nil
            self.loaded = false
        }
    }

    /// Where libspire_code.dylib is looked for, in order:
    ///
    /// 1) Bundled app:  Spire.app/Contents/Frameworks/libspire_code.dylib
    /// 2) Dev fallback: <repo-root>/target/release/libspire_code.dylib
    ///    (source file is at
    ///     .../spire-code/ui/swift/Sources/SpireUI/Bridge/SpireFFI.swift,
    ///     walk up 1:Bridge/ 2:SpireUI/ 3:Sources/ 4:swift/ 5:ui/ 6:spire-code/)
    static func candidatePaths() -> [String] {
        var candidates: [String] = []

        candidates.append(
            Bundle.main.bundleURL
                .appendingPathComponent("Contents")
                .appendingPathComponent("Frameworks")
                .appendingPathComponent("libspire_code.dylib")
                .path
        )

        candidates.append(
            URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()  // Bridge/
                .deletingLastPathComponent()  // SpireUI/
                .deletingLastPathComponent()  // Sources/
                .deletingLastPathComponent()  // swift/
                .deletingLastPathComponent()  // ui/
                .deletingLastPathComponent()  // spire-code/
                .appendingPathComponent("target")
                .appendingPathComponent("release")
                .appendingPathComponent("libspire_code.dylib")
                .path
        )

        return candidates
    }

    /// `dlopen` the core. `dlopen` is refcounted, so resolving more than once is
    /// intentional and cheap — `init` and the static `configDir()` both need it.
    static func resolveHandle() -> (handle: UnsafeMutableRawPointer, path: String)? {
        for path in candidatePaths() {
            if let h = dlopen(path, RTLD_NOW | RTLD_LOCAL) {
                return (h, path)
            }
        }
        return nil
    }

    /// The core's resolved per-application config directory
    /// (e.g. `~/.spire/spire-code`), or nil when the core is unavailable.
    ///
    /// Asked of the core rather than re-derived here, so the host and the core
    /// cannot disagree about the scope.
    static func configDir() -> String? {
        guard let (h, _) = resolveHandle() else { return nil }
        guard let dirSym = dlsym(h, "spire_config_dir") else { return nil }
        let freeSym = dlsym(h, "spire_free_string")
        let dirFn = unsafeBitCast(dirSym, to: (@convention(c) () -> UnsafeMutablePointer<CChar>?).self)
        let freeFn = unsafeBitCast(freeSym, to: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void).self)
        guard let ptr = dirFn() else { return nil }
        defer { freeFn(ptr) }
        return String(cString: ptr)
    }

    /// Block until the next pushed file-change event is available (or timeout).
    /// Returns the event JSON string, or nil on timeout.
    func waitForEvent(timeoutMs: UInt32) -> String? {
        guard loaded, let h = handle else { return nil }
        guard let waitSym = dlsym(h, "spire_wait_for_event") else { return nil }
        let freeSym = dlsym(h, "spire_free_string")
        let waitFn = unsafeBitCast(waitSym, to: (@convention(c) (UInt32) -> UnsafeMutablePointer<CChar>?).self)
        let freeFn = unsafeBitCast(freeSym, to: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void).self)
        guard let ptr = waitFn(timeoutMs) else { return nil }
        defer { freeFn(ptr) }
        return String(cString: ptr)
    }

    func drainBuildEvents() -> String? {
        guard loaded, let h = handle else { return nil }
        guard let drainSym = dlsym(h, "spire_drain_build_events") else { return nil }
        let freeSym = dlsym(h, "spire_free_string")
        let drainFn = unsafeBitCast(drainSym, to: (@convention(c) () -> UnsafeMutablePointer<CChar>?).self)
        let freeFn = unsafeBitCast(freeSym, to: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void).self)
        guard let ptr = drainFn() else { return nil }
        defer { freeFn(ptr) }
        return String(cString: ptr)
    }

    func waitForBuildEvent(timeoutMs: UInt32) -> String? {
        guard loaded, let h = handle else { return nil }
        guard let waitSym = dlsym(h, "spire_wait_for_build_event") else { return nil }
        let freeSym = dlsym(h, "spire_free_string")
        let waitFn = unsafeBitCast(waitSym, to: (@convention(c) (UInt32) -> UnsafeMutablePointer<CChar>?).self)
        let freeFn = unsafeBitCast(freeSym, to: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void).self)
        guard let ptr = waitFn(timeoutMs) else { return nil }
        defer { freeFn(ptr) }
        return String(cString: ptr)
    }

    deinit {
        if let h = handle {
            dlclose(h)
        }
    }

    /// Send a JSON command and receive a JSON response.
    func send(_ data: Data) async throws -> Data {
        guard loaded, let h = handle else {
            throw MessageError.notImplemented
        }

        // Resolve function pointers
        guard let sendJsonSym = dlsym(h, "spire_send_json") else {
            throw MessageError.notImplemented
        }
        guard let freeStrSym = dlsym(h, "spire_free_string") else {
            throw MessageError.notImplemented
        }

        let sendJsonFn = unsafeBitCast(sendJsonSym, to: (@convention(c) (UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?).self)
        let freeStrFn = unsafeBitCast(freeStrSym, to: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void).self)

        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let requestStr = String(data: data, encoding: .utf8) ?? ""
                let responsePtr = requestStr.withCString { cstr in
                    sendJsonFn(cstr)
                }

                guard let ptr = responsePtr else {
                    continuation.resume(throwing: MessageError.invalidMessage)
                    return
                }

                let responseStr = String(cString: ptr)
                freeStrFn(ptr)

                guard let responseData = responseStr.data(using: .utf8) else {
                    continuation.resume(throwing: MessageError.decodingFailed("UTF-8 decode failed"))
                    return
                }

                continuation.resume(returning: responseData)
            }
        }
    }
}

/// Paths the host writes that belong to the core's scope.
///
/// The core owns `~/.spire/<app>/` (see `spire-core`'s `config`), so the host
/// places its own files beside the core's instead of re-deriving the scope.
/// Resolved once, from the core; falls back to the pre-scope `~/.spire` when the
/// core is unavailable, so the app still starts without the dylib.
enum SpireCorePaths {
    static let configDir: String = SpireFFIBackend.configDir()
        ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".spire").path
}