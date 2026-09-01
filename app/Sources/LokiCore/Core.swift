import CLoki
import Foundation

/// One event from the core.
///
/// Carries the raw JSON rather than a decoded dictionary so the type stays `Sendable`.
/// Decode on demand with ``fields()``.
public struct CoreEvent: Sendable {
    /// The `event` tag, for example `task_started`.
    public let kind: String
    /// The full payload as it arrived.
    public let json: String

    init?(json: String) {
        guard let data = json.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let kind = object["event"] as? String
        else { return nil }
        self.kind = kind
        self.json = json
    }

    public func fields() -> [String: Any] {
        guard let data = json.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return [:] }
        return object
    }
}

public enum Provider: Sendable {
    case anthropic
    case openai

    var raw: LokiProvider {
        switch self {
        case .anthropic: LOKI_PROVIDER_ANTHROPIC
        case .openai: LOKI_PROVIDER_OPENAI
        }
    }
}

public enum CoreError: Error {
    case couldNotStart
    case invalidArgument
    case notReady
    case unsupported

    init?(_ status: LokiStatus) {
        switch status {
        case LOKI_OK: return nil
        case LOKI_INVALID_ARGUMENT: self = .invalidArgument
        case LOKI_NOT_READY: self = .notReady
        default: self = .unsupported
        }
    }
}

/// Swift side of the bridge.
///
/// Owns the Rust core and wraps the C ABI so nothing above this touches a raw pointer.
///
/// Callbacks arrive on a Rust worker thread and are yielded straight into an `AsyncStream`.
/// Yielding is thread-safe and needs no actor hop, so the core stays nonisolated and callers
/// consume the streams on whatever isolation they want.
public final class Core: Sendable {
    /// Everything the core does, in order.
    public let events: AsyncStream<CoreEvent>
    /// Response text as it streams.
    public let tokens: AsyncStream<String>

    // Safety invariant: set once in init and never reassigned, every C function it is passed to
    // is internally synchronized on the Rust side (a tokio Mutex around the loop, a std Mutex
    // around the cancellation token), and the core is freed exactly once in deinit, after which
    // no method can run. Marked unsafe only because OpaquePointer carries no Sendable
    // conformance. Removable if the handle is ever wrapped in a Sendable Rust-side type.
    private nonisolated(unsafe) let handle: OpaquePointer
    private let sinks: Unmanaged<Sinks>

    /// The linked core's version. Available without starting a core.
    public static var version: String {
        guard let ptr = loki_version() else { return "unavailable" }
        defer { loki_string_free(ptr) }
        return String(cString: ptr)
    }

    /// - Parameter model: the provider's default when nil.
    public init(provider: Provider, apiKey: String, model: String? = nil) throws {
        var eventContinuation: AsyncStream<CoreEvent>.Continuation!
        let events = AsyncStream<CoreEvent>(bufferingPolicy: .unbounded) {
            eventContinuation = $0
        }
        var tokenContinuation: AsyncStream<String>.Continuation!
        let tokens = AsyncStream<String>(bufferingPolicy: .unbounded) {
            tokenContinuation = $0
        }

        let sinks = Unmanaged.passRetained(
            Sinks(events: eventContinuation, tokens: tokenContinuation)
        )

        let created = apiKey.withCString { key in
            if let model {
                model.withCString { m in
                    loki_core_new(provider.raw, key, m, eventBridge, tokenBridge, sinks.toOpaque())
                }
            } else {
                loki_core_new(provider.raw, key, nil, eventBridge, tokenBridge, sinks.toOpaque())
            }
        }

        guard let handle = created else {
            sinks.release()
            throw CoreError.couldNotStart
        }

        self.events = events
        self.tokens = tokens
        self.handle = handle
        self.sinks = sinks
    }

    deinit {
        // Order matters. Freeing the core stops Rust calling back, so the sinks must outlive it.
        loki_core_free(handle)
        sinks.takeUnretainedValue().finish()
        sinks.release()
    }

    /// Starts a turn. Returns immediately; output arrives on ``tokens`` and ``events``.
    public func send(_ text: String) throws {
        let status = text.withCString { loki_send_message(handle, $0) }
        if let error = CoreError(status) { throw error }
    }

    /// Stops the running turn.
    public func interrupt() throws {
        if let error = CoreError(loki_interrupt(handle)) { throw error }
    }

    /// Spend today, in millionths of a cent. Zero if the ledger is unavailable.
    public var spentToday: UInt64 { loki_spend_today(handle) }

    /// Spend this calendar month, in millionths of a cent.
    public var spentThisMonth: UInt64 { loki_spend_month(handle) }

    /// Adds an instruction that compaction can never remove.
    public func addStanding(_ text: String, persistent: Bool = false) throws {
        let status = text.withCString { loki_add_standing(handle, $0, persistent) }
        if let error = CoreError(status) { throw error }
    }
}

/// Continuations reachable from the C callbacks.
///
/// Immutable and holding only `Sendable` continuations, so it crosses the boundary safely.
/// Yielding into an `AsyncStream` is thread-safe by contract.
private final class Sinks: Sendable {
    let events: AsyncStream<CoreEvent>.Continuation
    let tokens: AsyncStream<String>.Continuation

    init(
        events: AsyncStream<CoreEvent>.Continuation,
        tokens: AsyncStream<String>.Continuation
    ) {
        self.events = events
        self.tokens = tokens
    }

    func finish() {
        events.finish()
        tokens.finish()
    }
}

private func eventBridge(_ json: UnsafePointer<CChar>?, _ userData: UnsafeMutableRawPointer?) {
    guard let json, let userData, let event = CoreEvent(json: String(cString: json)) else { return }
    Unmanaged<Sinks>.fromOpaque(userData).takeUnretainedValue().events.yield(event)
}

private func tokenBridge(_ text: UnsafePointer<CChar>?, _ userData: UnsafeMutableRawPointer?) {
    guard let text, let userData else { return }
    Unmanaged<Sinks>.fromOpaque(userData).takeUnretainedValue().tokens.yield(String(cString: text))
}
