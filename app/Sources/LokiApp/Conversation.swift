import Foundation
import LokiCore
import Observation

/// One turn in the thread.
struct Turn: Identifiable {
    enum Speaker { case user, assistant }

    let id = UUID()
    let speaker: Speaker
    var text: String
}

/// One step inside a scope, as the rail renders it.
struct Step: Identifiable {
    let id = UUID()
    let verb: String
    let detail: String
}

/// A stretch of work that held resources. Renders as a rail in the left gutter.
struct Scope: Identifiable {
    let id: UInt64
    var kind: String
    var state: Theme.State
    var steps: [Step] = []
    var elapsed: UInt64?
}

/// What the composer is doing.
enum ComposerState {
    case idle
    case listening
    case running
    case needsYou

    var border: Theme.State? {
        switch self {
        case .idle: nil
        case .listening: .reading
        case .running: .holding
        case .needsYou: .needsYou
        }
    }
}

/// One item in the thread, in the order it happened.
///
/// Turns and scopes share a timeline. Rendering them as two lists puts every rail below every
/// turn, which loses the thing the rail is for: showing what happened while an answer was formed.
enum Entry: Identifiable {
    case turn(Turn)
    case scope(Scope)

    var id: String {
        switch self {
        case .turn(let turn): "turn-\(turn.id)"
        case .scope(let scope): "scope-\(scope.id)"
        }
    }
}

/// The thread's state, driven by the core's event stream.
@MainActor
@Observable
final class Conversation {
    private(set) var entries: [Entry] = []
    private(set) var composer: ComposerState = .idle
    private(set) var spentCents: UInt64 = 0
    private(set) var lastError: String?

    let dictation = Dictation()

    private let core: Core?
    private var streaming: Turn.ID?

    /// Reads the provider and key from the environment until `SecretStore` lands in Phase 4.
    ///
    /// `LOKI_PROVIDER` picks between `anthropic` and `openai` when both keys are present.
    /// `LOKI_MODEL` overrides the provider's default model.
    init() {
        let environment = ProcessInfo.processInfo.environment
        let model = environment["LOKI_MODEL"].flatMap { $0.isEmpty ? nil : $0 }
        let anthropic = environment["ANTHROPIC_API_KEY"].flatMap { $0.isEmpty ? nil : $0 }
        let openai = environment["OPENAI_API_KEY"].flatMap { $0.isEmpty ? nil : $0 }

        switch environment["LOKI_PROVIDER"]?.lowercased() {
        case "openai":
            core = openai.flatMap { try? Core(provider: .openai, apiKey: $0, model: model) }
            if core == nil { lastError = "LOKI_PROVIDER is openai but OPENAI_API_KEY is not set." }
        case "anthropic":
            core = anthropic.flatMap { try? Core(provider: .anthropic, apiKey: $0, model: model) }
            if core == nil {
                lastError = "LOKI_PROVIDER is anthropic but ANTHROPIC_API_KEY is not set."
            }
        default:
            // No preference stated. Whichever key exists, Anthropic first.
            if let key = anthropic {
                core = try? Core(provider: .anthropic, apiKey: key, model: model)
            } else if let key = openai {
                core = try? Core(provider: .openai, apiKey: key, model: model)
            } else {
                core = nil
                lastError = "No model key. Set ANTHROPIC_API_KEY or OPENAI_API_KEY and relaunch."
            }
        }
    }

    var isReady: Bool { core != nil }

    /// Speaking during a task is an interrupt.
    ///
    /// Voice activity detection fires this before transcription finishes, so the visible stop
    /// lands inside the 150ms budget rather than waiting for words.
    private func speechStarted() {
        if case .running = composer { interrupt() }
    }

    /// Begins an utterance. The composer shows `listening` while it runs.
    func startDictation() {
        dictation.onSpeechStart = { [weak self] in self?.speechStarted() }
        composer = .listening
        Task { await dictation.start() }
    }

    /// Ends the utterance and returns what was said, for the composer to place in the draft.
    func stopDictation() async -> String {
        let text = await dictation.stop()
        if case .listening = composer { composer = .idle }
        return text
    }

    /// Starts consuming the core's two streams. Called once, when the thread appears.
    func observe() {
        guard let core else { return }

        Task { [weak self] in
            for await event in core.events {
                self?.apply(event)
            }
        }
        Task { [weak self] in
            for await token in core.tokens {
                self?.append(token)
            }
        }
    }

    func send(_ text: String) {
        guard let core else { return }
        entries.append(.turn(Turn(speaker: .user, text: text)))
        lastError = nil
        composer = .running
        do {
            try core.send(text)
        } catch {
            lastError = String(describing: error)
            composer = .idle
        }
    }

    func interrupt() {
        try? core?.interrupt()
    }

    /// Turns only, for the popover's session count.
    var turns: [Turn] {
        entries.compactMap { if case .turn(let turn) = $0 { turn } else { nil } }
    }

    private func append(_ token: String) {
        if let id = streaming, let index = indexOfTurn(id) {
            guard case .turn(var turn) = entries[index] else { return }
            turn.text += token
            entries[index] = .turn(turn)
        } else {
            let turn = Turn(speaker: .assistant, text: token)
            streaming = turn.id
            entries.append(.turn(turn))
        }
    }

    private func indexOfTurn(_ id: Turn.ID) -> Int? {
        entries.firstIndex { if case .turn(let t) = $0 { t.id == id } else { false } }
    }

    private func indexOfScope(_ id: UInt64) -> Int? {
        entries.firstIndex { if case .scope(let s) = $0 { s.id == id } else { false } }
    }

    private func updateScope(_ id: UInt64, _ change: (inout Scope) -> Void) {
        guard let index = indexOfScope(id), case .scope(var scope) = entries[index] else { return }
        change(&scope)
        entries[index] = .scope(scope)
    }

    /// The most recent scope, which is where a step belongs.
    private func appendStep(_ step: Step) {
        guard let index = entries.lastIndex(where: { if case .scope = $0 { true } else { false } }),
              case .scope(var scope) = entries[index]
        else { return }
        scope.steps.append(step)
        entries[index] = .scope(scope)
    }

    private func apply(_ event: CoreEvent) {
        let fields = event.fields()

        switch event.kind {
        case "task_started":
            streaming = nil

        case "scope_opened":
            guard let id = fields["id"] as? UInt64 else { return }
            entries.append(
                .scope(Scope(id: id, kind: fields["kind"] as? String ?? "tool", state: .reading))
            )

        case "scope_closed":
            guard let id = fields["id"] as? UInt64 else { return }
            updateScope(id) { scope in
                scope.state = .released
                scope.elapsed = fields["ms"] as? UInt64
            }

        case "tool_called":
            guard let tool = fields["tool"] as? String else { return }
            if fields["tier"] as? String == "irreversible" { composer = .needsYou }
            appendStep(Step(verb: "call", detail: tool))

        case "memory_recalled":
            let count = (fields["concept_ids"] as? [String])?.count ?? 0
            appendStep(Step(verb: "recall", detail: "\(count) concepts"))

        case "budget_warning":
            if let spent = fields["spent"] as? UInt64 { spentCents = spent }

        case "blocked":
            composer = .needsYou
            lastError = describe(fields["reason"])

        case "interrupted":
            composer = .idle
            streaming = nil
            markOpenScopesInterrupted()

        case "task_finished":
            composer = .idle
            streaming = nil
            // A blocked event already said why. Only speak up if nothing did.
            if fields["status"] as? String == "failed", lastError == nil {
                lastError = "That did not work."
            }

        default:
            break
        }
    }

    private func markOpenScopesInterrupted() {
        for index in entries.indices {
            guard case .scope(var scope) = entries[index], scope.state != .released else { continue }
            scope.state = .needsYou
            entries[index] = .scope(scope)
        }
    }

    /// Turns a `BlockReason` into a sentence. The Rust side already phrased the detail.
    private func describe(_ reason: Any?) -> String {
        guard let reason = reason as? [String: Any],
              let kind = reason.keys.first,
              let body = reason[kind] as? [String: Any]
        else { return "Stopped." }

        switch kind {
        case "provider_failed":
            let provider = body["provider"] as? String ?? "The provider"
            let detail = body["detail"] as? String ?? "no detail"
            return "\(provider) could not answer: \(detail)"
        case "budget_ceiling":
            let spent = body["spent"] as? UInt64 ?? 0
            return "Paused at your spending limit, \(spent) cents used."
        case "awaiting_confirm":
            return "Waiting on you before \(body["action"] as? String ?? "that")."
        case "auth_expired":
            return "The connection to \(body["connector"] as? String ?? "that") expired."
        default:
            return "Stopped."
        }
    }
}
