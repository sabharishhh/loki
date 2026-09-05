import Foundation
import SwiftUI
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
    /// What the step returned, when it returned something worth reading.
    var output: String?
}

/// A stretch of work that held resources. Renders as a rail in the left gutter.
struct Scope: Identifiable {
    let id: UInt64
    var kind: String
    var state: Theme.State
    var steps: [Step] = []
    var elapsed: UInt64?
    /// Depth in the scope tree. A nested scope is indented under its parent so a code-mode
    /// script's calls read as its own steps rather than as a flat list (§13.3).
    var depth: Int = 0
    /// A turn cut short. The rail keeps a mark rather than vanishing, because what was kept and
    /// what was dropped is exactly what a user needs after an interrupt (§18.3).
    var interrupted = false
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
        case .running: .thinking
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
    /// Spend today, in millionths of a cent. Refreshed when a turn ends.
    private(set) var spentToday: UInt64 = 0
    /// What this session has spent in tokens (§21.3). Read off the same events as the cost.
    private(set) var tokens = SessionTokens.zero
    private(set) var lastError: String?
    /// What memory put in play for the last turn, for the rail.
    private(set) var recalled: [RecalledClaim] = []
    /// Up to three lines at session close, and nothing when nothing happened (§17.4).
    private(set) var summary: [String] = []
    /// Whether the turn now running asked Loki to remember something (§8.1).
    private var captureWhenDone = false
    /// Set while a capture is in flight, so two turns cannot consolidate at once.
    private var capturing = false

    /// The per-turn cap the rail counts against. Mirrors `RECALL_CAP` in the core.
    static let recallCap = 5

    let dictation = Dictation()

    private let core: Core?
    private var streaming: Turn.ID?
    /// Evens out the provider's bursts. See `Streaming.swift` for why the view does not read the
    /// token stream directly. Built in `observe`, alongside the stream it smooths.
    @ObservationIgnored private var streamer: Streamer?

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
            if core == nil {
                lastError = "LOKI_PROVIDER is openai but OPENAI_API_KEY is not set. "
                    + "In a terminal it must be on the same line as the command, or exported."
            }
        case "anthropic":
            core = anthropic.flatMap { try? Core(provider: .anthropic, apiKey: $0, model: model) }
            if core == nil {
                lastError = "LOKI_PROVIDER is anthropic but ANTHROPIC_API_KEY is not set. "
                    + "In a terminal it must be on the same line as the command, or exported."
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

    /// Past sessions, newest first, for the sidebar.
    func sessions() -> [String] {
        core?.sessions() ?? []
    }

    /// The memory timeline, newest first.
    func timeline() -> [String] {
        core?.timeline() ?? []
    }

    /// What Loki knows, grouped by the thing it is about (§17.3).
    func knowledge() -> Knowledge {
        core?.knowledge() ?? Knowledge(entities: [])
    }

    /// Confirms which side of a conflict is right (§9.7 rule 4).
    ///
    /// The store already decided, using the later statement, and kept the other to be checked.
    /// This makes the choice permanent and pins the concept against decay.
    func settle(path: String, keep: UInt32) {
        try? core?.settle(path: path, keep: keep)
    }

    /// Drops one of the other names an entity answers to.
    func forgetAlias(path: String, form: String) {
        try? core?.forgetAlias(path: path, form: form)
    }

    /// Closes an edge. It was true until now, so it is closed rather than deleted.
    func forgetRelation(path: String, label: String, to: String) {
        try? core?.forgetRelation(path: path, label: label, to: to)
    }

    /// Folds one card into another, on the user's word. Never automatic.
    func merge(from: String, into: String) {
        try? core?.merge(from: from, into: into)
    }

    /// Replaces what a claim says. A supersession, not an overwrite.
    func amend(path: String, ordinal: UInt32, text: String) {
        try? core?.amend(path: path, ordinal: ordinal, text: text)
    }

    /// Retires a claim with nothing in its place. Retired, never removed.
    func forget(path: String, ordinal: UInt32) {
        try? core?.forget(path: path, ordinal: ordinal)
    }

    /// Marks a recalled claim wrong, and drops it from the rail so the tap has a visible effect.
    ///
    /// Nothing is deleted. Confidence collapses and the claim is flagged, which is recoverable if
    /// the click was a mistake.
    func markNotTrue(_ claim: RecalledClaim) {
        guard !claim.fromSession else { return }
        try? core?.notTrue(path: claim.path, ordinal: claim.ordinal)
        withAnimation(Theme.Motion.control) {
            recalled.removeAll { $0.id == claim.id }
        }
    }

    /// Captures now, because the user asked Loki to remember something (§8.1).
    ///
    /// Runs after the reply rather than before it, so the answer is never held up by a pass whose
    /// result the answer does not need.
    private func capture() async {
        guard !capturing else { return }
        capturing = true
        await endSession()
        capturing = false
    }

    /// Consolidates the session and shows the summary, if there is one worth showing.
    ///
    /// Silence when nothing happened: a card that says "learned nothing today" teaches people to
    /// ignore the card.
    func endSession() async {
        guard let core else { return }
        let lines = await Task.detached { core.endSession() }.value
        withAnimation(Theme.Motion.control) { summary = lines }
    }

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
        let streamer = Streamer { [weak self] text in self?.show(text) }
        self.streamer = streamer
        Task {
            for await token in core.tokens {
                streamer.accept(token)
            }
        }
    }

    func send(_ text: String) {
        guard let core else { return }
        entries.append(.turn(Turn(speaker: .user, text: text)))
        lastError = nil
        composer = .running
        // §8.1's exception: an explicit instruction to remember applies to this session, not the
        // next one. Everything else waits for session close, because a model call per turn buys
        // little on turns that contain nothing durable.
        captureWhenDone = Conversation.isExplicitInstruction(text)
        do {
            try core.send(text)
        } catch {
            lastError = String(describing: error)
            composer = .idle
        }
    }

    /// Whether a message is an instruction to remember something, rather than a passing remark.
    ///
    /// Deliberately literal. A heuristic that fires too widely turns every turn into a model call,
    /// and one that guesses at intent would capture things the user did not ask to keep. When it
    /// misses, the fact is still captured at session close, so the cost of a miss is a delay
    /// rather than a loss.
    static func isExplicitInstruction(_ text: String) -> Bool {
        let lowered = text.lowercased()
        let phrases = [
            "remember", "note that", "keep in mind", "don't forget", "do not forget",
            "for future reference", "from now on", "make a note",
        ]
        return phrases.contains { lowered.contains($0) }
    }

    func interrupt() {
        try? core?.interrupt()
    }

    /// Turns only, for the popover's session count.
    var turns: [Turn] {
        entries.compactMap { if case .turn(let turn) = $0 { turn } else { nil } }
    }

    /// Puts a smoothed batch of characters on screen. Only the streamer calls this.
    private func show(_ text: String) {
        if let id = streaming, let index = indexOfTurn(id) {
            guard case .turn(var turn) = entries[index] else { return }
            turn.text += text
            entries[index] = .turn(turn)
        } else {
            let turn = Turn(speaker: .assistant, text: text)
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
            streamer?.reset()
            streaming = nil

        case "scope_opened":
            guard let id = fields["id"] as? UInt64 else { return }
            // A scope opened inside another is one level deeper. The core sends the parent, so
            // the depth is read rather than guessed from arrival order.
            let parent = fields["parent"] as? UInt64
            let depth = parent.flatMap { id in
                entries.compactMap { entry -> Int? in
                    if case let .scope(scope) = entry, scope.id == id { return scope.depth + 1 }
                    return nil
                }.last
            } ?? 0
            entries.append(
                .scope(
                    Scope(
                        id: id,
                        kind: fields["kind"] as? String ?? "tool",
                        state: .reading,
                        depth: depth
                    )
                )
            )

        case "scope_closed":
            guard let id = fields["id"] as? UInt64 else { return }
            updateScope(id) { scope in
                scope.state = .idle
                scope.elapsed = fields["ms"] as? UInt64
            }

        case "tool_called":
            guard let tool = fields["tool"] as? String else { return }
            if fields["tier"] as? String == "irreversible" { composer = .needsYou }
            appendStep(Step(verb: "call", detail: tool, output: fields["args"] as? String))

        case "tool_returned":
            // The output lands on the step that called it, so the well sits under its own row
            // rather than at the end of the scope.
            let summary = fields["summary"] as? String
            attachOutput(summary)

        case "memory_recalled":
            // `claim_ids`, and each one is an object rather than a string. Reading the wrong key
            // made every turn say it recalled nothing, whatever it had actually found (B-46).
            let count = (fields["claim_ids"] as? [Any])?.count ?? 0
            // Lane 2 returns file lines rather than addressed claims, so it never has ids to
            // count. Saying "0 facts" for a search that found plenty would be worse than silence.
            if fields["lane"] as? String == "deliberate" {
                appendStep(Step(verb: "search", detail: "memory"))
            } else {
                appendStep(Step(verb: "recall", detail: count == 1 ? "1 fact" : "\(count) facts"))
            }

        case "budget_warning":
            refreshSpend()

        case "blocked":
            composer = .needsYou
            lastError = describe(fields["reason"])

        case "interrupted":
            composer = .idle
            // Everything already received goes up before the mark. §18.3 puts the cut where the
            // text stopped, and a mark above text still arriving points at the wrong place.
            streamer?.flush()
            markOpenScopesInterrupted()
            markCut()

        case "task_finished":
            composer = .idle
            // Keeps flowing until the backlog is empty rather than cutting to the end, so an
            // answer does not finish with a jump. `streaming` is cleared by the next
            // `task_started`, or the tail would start a turn of its own.
            streamer?.finish()
            // Event-driven, not polled. Principle 8 forbids a timer for this.
            refreshSpend()
            refreshRecalled()
            if captureWhenDone {
                captureWhenDone = false
                Task { await capture() }
            }
            // A blocked event already said why. Only speak up if nothing did.
            if fields["status"] as? String == "failed", lastError == nil {
                lastError = "That did not work."
            }

        default:
            break
        }
    }

    private func refreshSpend() {
        spentToday = core?.spentToday ?? 0
        if let tokens = core?.sessionTokens() { self.tokens = tokens }
    }

    /// Reads what pre-fetch used on the turn that just finished.
    ///
    /// Read after the turn rather than before it, because the rail's job is to show what the
    /// answer was actually built from, not what was on offer.
    private func refreshRecalled() {
        let found = core?.recalled ?? []
        withAnimation(Theme.Motion.control) { recalled = found }
    }

    /// Marks the cut, so the thread shows where the turn stopped rather than just stopping.
    /// Attaches output to the most recent step of the open scope.
    private func attachOutput(_ output: String?) {
        guard let output, !output.isEmpty else { return }
        for index in entries.indices.reversed() {
            guard case .scope(var scope) = entries[index], !scope.steps.isEmpty else { continue }
            scope.steps[scope.steps.count - 1].output = output
            entries[index] = .scope(scope)
            return
        }
    }

    private func markCut() {
        for index in entries.indices {
            guard case .scope(var scope) = entries[index], scope.state != .idle else { continue }
            scope.interrupted = true
            entries[index] = .scope(scope)
        }
    }

    private func markOpenScopesInterrupted() {
        for index in entries.indices {
            guard case .scope(var scope) = entries[index], scope.state != .idle else { continue }
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
