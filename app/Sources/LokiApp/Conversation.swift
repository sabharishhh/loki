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

/// The thread's state, driven by the core's event stream.
@MainActor
@Observable
final class Conversation {
    private(set) var turns: [Turn] = []
    private(set) var scopes: [Scope] = []
    private(set) var composer: ComposerState = .idle
    private(set) var spentCents: UInt64 = 0
    private(set) var lastError: String?

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
        turns.append(Turn(speaker: .user, text: text))
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

    private func append(_ token: String) {
        if let id = streaming, let index = turns.firstIndex(where: { $0.id == id }) {
            turns[index].text += token
        } else {
            let turn = Turn(speaker: .assistant, text: token)
            streaming = turn.id
            turns.append(turn)
        }
    }

    private func apply(_ event: CoreEvent) {
        let fields = event.fields()

        switch event.kind {
        case "task_started":
            streaming = nil

        case "scope_opened":
            guard let id = fields["id"] as? UInt64 else { return }
            scopes.append(
                Scope(id: id, kind: fields["kind"] as? String ?? "tool", state: .reading)
            )

        case "scope_closed":
            guard let id = fields["id"] as? UInt64,
                  let index = scopes.firstIndex(where: { $0.id == id })
            else { return }
            scopes[index].state = .released
            scopes[index].elapsed = fields["ms"] as? UInt64

        case "tool_called":
            guard let tool = fields["tool"] as? String else { return }
            let tier = fields["tier"] as? String
            if tier == "irreversible" { composer = .needsYou }
            scopes.indices.last.map {
                scopes[$0].steps.append(Step(verb: "call", detail: tool))
            }

        case "memory_recalled":
            let count = (fields["concept_ids"] as? [String])?.count ?? 0
            scopes.indices.last.map {
                scopes[$0].steps.append(Step(verb: "recall", detail: "\(count) concepts"))
            }

        case "model_call":
            spentCents += 0

        case "budget_warning":
            if let spent = fields["spent"] as? UInt64 { spentCents = spent }

        case "blocked":
            composer = .needsYou

        case "interrupted":
            composer = .idle
            streaming = nil
            markOpenScopesInterrupted()

        case "task_finished":
            composer = .idle
            streaming = nil
            if fields["status"] as? String == "failed" {
                lastError = "That did not work."
            }

        default:
            break
        }
    }

    private func markOpenScopesInterrupted() {
        for index in scopes.indices where scopes[index].state != .released {
            scopes[index].state = .needsYou
        }
    }
}
