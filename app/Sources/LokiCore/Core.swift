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

    /// What memory contributed to the last turn. Empty when it contributed nothing.
    public var recalled: [RecalledClaim] {
        decode(loki_recalled(handle)) ?? []
    }

    /// The memory timeline, newest first.
    public func timeline(limit: Int = 200) -> [String] {
        decode(loki_timeline(handle, UInt32(limit))) ?? []
    }

    /// Past sessions, newest first.
    public func sessions(limit: Int = 60) -> [String] {
        decode(loki_sessions(handle, UInt32(limit))) ?? []
    }

    /// What this session has spent in tokens.
    public func sessionTokens() -> SessionTokens {
        decode(loki_session_tokens(handle)) ?? .zero
    }

    /// Where the session transcript is written, for the interface to point at.
    public static var journalPath: String {
        guard let pointer = loki_journal_path() else { return "" }
        defer { loki_string_free(pointer) }
        return String(cString: pointer)
    }

    /// What Loki knows, grouped by the thing it is about.
    ///
    /// The trust surface reads this rather than the timeline sentences: a log answers what
    /// changed, and the screen has to answer what Loki thinks it knows.
    public func knowledge() -> Knowledge {
        decode(loki_knowledge(handle)) ?? Knowledge(entities: [])
    }

    /// Drops one of the other names an entity answers to.
    public func forgetAlias(path: String, form: String) throws {
        let status = path.withCString { path in
            form.withCString { loki_forget_alias(handle, path, $0) }
        }
        if let error = CoreError(status) { throw error }
    }

    /// Closes an edge. Closed rather than deleted: it was true until now.
    public func forgetRelation(path: String, label: String, to: String) throws {
        let status = path.withCString { path in
            label.withCString { label in
                to.withCString { loki_forget_relation(handle, path, label, $0) }
            }
        }
        if let error = CoreError(status) { throw error }
    }

    /// Folds one card into another, on the user's word.
    ///
    /// Never automatic. A wrong merge hides a true fact where a split only leaves two rows, so
    /// this runs because somebody looked at both cards and said yes.
    public func merge(from: String, into: String) throws {
        let status = from.withCString { from in
            into.withCString { loki_merge_entities(handle, from, $0) }
        }
        if let error = CoreError(status) { throw error }
    }

    /// Confirms which side of a conflict is right, keeping the claim at `ordinal`.
    public func settle(path: String, keep: UInt32) throws {
        let status = path.withCString { loki_resolve_conflict(handle, $0, keep) }
        if let error = CoreError(status) { throw error }
    }

    /// Replaces what a claim says. A supersession, not an overwrite.
    public func amend(path: String, ordinal: UInt32, text: String) throws {
        let status = path.withCString { path in
            text.withCString { loki_amend_claim(handle, path, ordinal, $0) }
        }
        if let error = CoreError(status) { throw error }
    }

    /// Retires a claim with nothing in its place. Retired, never removed.
    public func forget(path: String, ordinal: UInt32) throws {
        let status = path.withCString { loki_forget_claim(handle, $0, ordinal) }
        if let error = CoreError(status) { throw error }
    }

    /// Marks a recalled claim wrong. Drops its confidence; deletes nothing.
    public func notTrue(path: String, ordinal: UInt32) throws {
        let status = path.withCString { loki_not_true(handle, $0, ordinal) }
        if let error = CoreError(status) { throw error }
    }

    /// Consolidates the session and returns up to three summary lines.
    ///
    /// Empty when nothing was learned, which is the design rather than a failure.
    public func endSession() -> [String] {
        decode(loki_end_session(handle)) ?? []
    }

    /// Decodes a JSON string the core allocated, and frees it either way.
    private func decode<T: Decodable>(_ pointer: UnsafeMutablePointer<CChar>?) -> T? {
        guard let pointer else { return nil }
        defer { loki_string_free(pointer) }
        guard let data = String(cString: pointer).data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }
}

/// What one session has spent in tokens (§21.3).
///
/// `context` is the last call's input rather than a sum: it says how big the prompt has grown,
/// which is the number worth watching. If it climbs across sessions, consolidation is letting
/// noise in.
public struct SessionTokens: Decodable, Sendable, Equatable {
    public let input: UInt64
    public let output: UInt64
    public let context: UInt64
    public let calls: UInt64

    /// Before the first call, and whenever the core is not there to ask.
    public static let zero = Self(input: 0, output: 0, context: 0, calls: 0)

    public init(input: UInt64, output: UInt64, context: UInt64, calls: UInt64) {
        self.input = input
        self.output = output
        self.context = context
        self.calls = calls
    }
}

/// Everything Loki knows, grouped by the thing it is about (§17.3).
public struct Knowledge: Decodable, Sendable, Equatable {
    public let entities: [KnownEntity]
    /// Cards that answer to one name. Derived on every read, so a split from any source shows up.
    public let duplicates: [Duplicate]

    public init(entities: [KnownEntity], duplicates: [Duplicate] = []) {
        self.entities = entities
        self.duplicates = duplicates
    }
}

/// Two or more cards claiming the same name, with the fuller one first.
public struct Duplicate: Decodable, Sendable, Equatable, Identifiable {
    public let form: String
    public let paths: [String]
    public let names: [String]

    public var id: String { form }
}

/// One person, project or preference, and what is known about it.
public struct KnownEntity: Decodable, Sendable, Equatable, Identifiable {
    public let path: String
    public let name: String
    public let kind: String
    /// Whether anything here can reach a prompt. The consequence, not the state name.
    public let inUse: Bool
    /// Confirmed by a person, so nothing decays it by heuristic.
    public let confirmed: Bool
    public let facts: [KnownFact]
    /// Other names this answers to. Knowledge, so it is shown rather than kept in a file.
    public let alsoKnownAs: [String]
    /// Live edges out of this entity.
    public let relations: [Related]

    public var id: String { path }

    private enum CodingKeys: String, CodingKey {
        case path, name, kind, facts, relations
        case inUse = "in_use"
        case alsoKnownAs = "also_known_as"
        case confirmed
    }
}

/// One current edge, as a row reads it.
public struct Related: Decodable, Sendable, Equatable, Identifiable {
    public let label: String
    public let name: String
    public let path: String

    public var id: String { "\(label)/\(path)" }
}

/// One thing Loki knows, with its own history folded in.
public struct KnownFact: Decodable, Sendable, Equatable, Identifiable {
    public let ordinal: UInt32
    public let attribute: String
    public let text: String
    /// `Since 15 July, about seven weeks.` Absent when the source never dated it.
    public let since: String?
    /// What this replaced, on the same row.
    public let was: Superseded?
    /// True when it came from a page or an account rather than from the user.
    public let fromElsewhere: Bool
    /// Other things said about this property that Loki is not using.
    ///
    /// Kept, never sent to the model, and offered back. Nothing blocks on them: an approval queue
    /// nobody works through is worse than a decision the user can see and flip.
    public let alsoSaid: [Alternative]

    public var id: UInt32 { ordinal }

    private enum CodingKeys: String, CodingKey {
        case ordinal, attribute, text, since, was
        case fromElsewhere = "from_elsewhere"
        case alsoSaid = "also_said"
    }
}

/// The half of a correction that is no longer true.
public struct Superseded: Decodable, Sendable, Equatable {
    public let text: String
    /// `from 1 March to 15 July`.
    public let held: String
    /// `about six weeks`, when Loki went on believing it after it had stopped being true.
    public let wrongFor: String?

    private enum CodingKeys: String, CodingKey {
        case text, held
        case wrongFor = "wrong_for"
    }
}

/// Something said about a property that a later statement overrode.
public struct Alternative: Decodable, Sendable, Equatable, Identifiable {
    public let ordinal: UInt32
    public let text: String
    public let since: String?

    public var id: UInt32 { ordinal }
}

/// One claim pre-fetch surfaced for a turn.
public struct RecalledClaim: Decodable, Sendable, Identifiable, Equatable {
    public let path: String
    public let name: String
    public let text: String
    public let ordinal: UInt32
    public let score: Double
    /// Whether it came from earlier in this same conversation rather than from stored memory.
    public let fromSession: Bool

    public var id: String { "\(path)#\(ordinal)" }
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
