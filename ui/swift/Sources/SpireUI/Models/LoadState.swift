import Foundation

/// Encodes the complete lifecycle of an async operation — the domain model's
/// loading state, not the view's concern. Views switch on this instead of
/// juggling optionals + separate loading booleans.
enum LoadingState<Value> {
    /// Initial state, no operation has started.
    case idle
    /// An operation is in progress.
    case loading
    /// The operation succeeded.
    case success(Value)
    /// The operation failed.
    case failure(Error)

    /// The loaded value if `.success`, otherwise nil.
    var value: Value? {
        if case .success(let v) = self { return v }
        return nil
    }

    /// True while an operation is in flight.
    var isLoading: Bool {
        if case .loading = self { return true }
        return false
    }

    /// True if a previous result is still available behind a refresh.
    /// Enables optimistic/cache-first rendering while a refresh runs.
    var hasValue: Bool {
        if case .success = self { return true }
        return false
    }
}
