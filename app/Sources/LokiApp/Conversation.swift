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
    /// When it landed. Shown on every turn, because a thread you come back to is a record and a
    /// record without times is a transcript of nothing in particular.
    var at = Date()
    /// The pages this answer was built from (§12.7). Empty when nothing was fetched.
    var sources: [Source] = []
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
    /// When the reader started waiting.
    ///
    /// The turn's start for the first scope of a turn, not the scope's own. Recall runs before
    /// the model call and opens no scope of its own, so counting from `ScopeOpened` leaves that
    /// time out of the only number the reader is shown.
    var began = ContinuousClock.now
    /// Depth in the scope tree. A nested scope is indented under its parent so a code-mode
    /// script's calls read as its own steps rather than as a flat list (§13.3).
    var depth: Int = 0
    /// A turn cut short. The rail keeps a mark rather than vanishing, because what was kept and
    /// what was dropped is exactly what a user needs after an interrupt (§18.3).
    var interrupted = false
    /// What a web search did, for the scopes that are one (§12.9). Kept apart from `steps` because
    /// it reads differently: a search says which pages it opened and how they answered, and that
    /// is a different shape from a tool call and its output.
    var search: [SearchStep] = []
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
    /// The sources the reader asked to see, which the rail shows.
    ///
    /// Held here rather than in either view because the thread raises the question and the rail
    /// answers it, and a value passed between two siblings has to live above both.
    private(set) var showingSources: [Source] = []
    /// Whether the turn now running asked Loki to remember something (§8.1).
    private var captureWhenDone = false
    /// Set while a capture is in flight, so two turns cannot consolidate at once.
    private var capturing = false

    /// A turn is under way: from the moment send is pressed until the answer is fully on screen.
    ///
    /// Deliberately wider than the core's task. It ends when the streamer has finished painting,
    /// not when the last token arrived, because the two differ by the whole length of the drain
    /// and the reader is waiting for the second one.
    private(set) var life = TurnLife()
    var working: Bool { life.working }
    /// When the reader started waiting, for the trace's clock.
    /// Where this turn's entries begin. Anything before it belongs to an earlier turn.
    @ObservationIgnored private var turnFloor = 0
    /// Steps reported before the turn opened a scope to hold them.
    @ObservationIgnored private var queuedSteps: [Step] = []
    /// The newest scope, so a trace can ask whether it is the one still running without every
    /// trace in the thread walking the thread to find out.

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
        streamer.onIdle = { [weak self] in self?.endTurn() }
        self.streamer = streamer
        Task {
            for await token in core.tokens {
                streamer.accept(token)
            }
        }
    }

    /// Sends a turn again after an edit, dropping everything that followed it.
    ///
    /// The answer to an edited question is not the answer to the question as edited, and keeping
    /// it would leave the thread reading as though Loki had replied to something nobody asked.
    func resend(from id: Turn.ID, text: String) {
        if let cut = entries.firstIndex(where: {
            if case .turn(let turn) = $0 { return turn.id == id }
            return false
        }) {
            entries.removeSubrange(cut...)
        }
        send(text)
    }

    /// Whether this turn is the one currently being written.
    ///
    /// Only ever the last assistant turn, and only while a turn is running. Asked per row rather
    /// than stored on the turn, so nothing has to remember to clear a flag.
    func isStreaming(_ turn: Turn) -> Bool {
        guard composer == .running, turn.speaker == .assistant else { return false }
        if case .turn(let last) = entries.last { return last.id == turn.id }
        return false
    }

    func send(_ text: String) {
        guard let core else { return }
        entries.append(.turn(Turn(speaker: .user, text: text)))
        beginTurn()
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
            life.end()
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

    /// Asks the rail to show these. Called when a source stack is clicked.
    /// Asking to see sources, counted rather than compared.
    ///
    /// **A value that has not changed is not an event.** Watching `showingSources` meant clicking
    /// the same stack twice did nothing the second time, so opening the rail, closing it and
    /// clicking again read as a dead control. The counter changes on every ask (W9).
    private(set) var sourcesAsked = 0

    func showSources(_ sources: [Source]) {
        withAnimation(Theme.Motion.control) {
            showingSources = sources
            sourcesAsked += 1
        }
    }

    func interrupt() {
        try? core?.interrupt()
    }

    /// Turns only, for the popover's session count.
    var turns: [Turn] {
        entries.compactMap { if case .turn(let turn) = $0 { turn } else { nil } }
    }

    /// Marks the start of a wait. Called on send, and again when the core says the task opened.
    ///
    /// Idempotent within a turn: the second call keeps the first clock, because the reader started
    /// waiting when they pressed send, not when the core got round to saying so.
    private func beginTurn() {
        life.begin()
        turnFloor = entries.count
        queuedSteps.removeAll()
    }

    /// The answer is fully on screen. Only the streamer decides this.
    private func endTurn() {
        life.end()
    }

    /// Whether this trace is still counting.
    ///
    /// A scope of the running turn stays live until the answer has finished painting, so the
    /// figure the reader is left with is the wait they actually had rather than the length of the
    /// model call inside it.
    func isLive(_ scope: Scope) -> Bool {
        life.isLive(scope: scope.id, closed: scope.state == .idle)
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

    /// The most recent scope of the running turn, which is where a step belongs.
    ///
    /// **Only this turn's.** Lane 1 recall runs before the model call, so its step is reported
    /// while the turn has no scope open. Taking the last scope in the thread filed it under the
    /// previous answer, where it read as something Loki had done for a question already answered,
    /// and on the first turn of a session it was dropped on the floor. Steps that arrive early
    /// wait for the scope that will hold them.
    /// Adds a step to the turn's search scope, and marks the one before it finished.
    ///
    /// A search reports what it did after each piece is done, so the row that was live becomes the
    /// row that is settled the moment the next one arrives.
    private func appendSearchStep(_ step: SearchStep, to kind: String = "search") {
        let index = entries.indices.dropFirst(turnFloor).last {
            if case let .scope(scope) = entries[$0] { return scope.kind == kind }
            return false
        }
        guard let index, case .scope(var scope) = entries[index] else { return }
        scope.search.advance(with: step)
        entries[index] = .scope(scope)
    }

    /// Which rung a `fetched` event came from, for the row that says a rung was not satisfied.
    private func rungNumber(_ fields: [String: Any]) -> Int {
        switch fields["rung"] as? String {
        case "direct": 1
        case "rendered": 2
        case "interactive": 3
        default: 1
        }
    }

    private func appendStep(_ step: Step) {
        // **Never into a retrieval scope.** Those render `search` and nothing else, so a thinking
        // step filed under one is a step that disappears. Lane 1's recall arrives before any scope
        // exists and waits for the model's, which is where it reads as thinking rather than as
        // something the memory search did.
        let index = entries.indices.dropFirst(turnFloor).last {
            if case let .scope(scope) = entries[$0] {
                return scope.kind != "search" && scope.kind != "memory"
            }
            return false
        }
        guard let index, case .scope(var scope) = entries[index] else {
            queuedSteps.append(step)
            return
        }
        scope.steps.append(step)
        entries[index] = .scope(scope)
    }

    private func apply(_ event: CoreEvent) {
        let fields = event.fields()

        switch event.kind {
        case "task_started":
            streamer?.reset()
            streaming = nil
            beginTurn()

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
            let firstOfTurn = !entries.indices.dropFirst(turnFloor).contains {
                if case .scope = entries[$0] { return true }
                return false
            }
            entries.append(
                .scope(
                    Scope(
                        id: id,
                        kind: fields["kind"] as? String ?? "tool",
                        state: .reading,
                        steps: firstOfTurn ? queuedSteps : [],
                        began: firstOfTurn ? (life.began ?? .now) : .now,
                        depth: depth
                    )
                )
            )
            life.opened(id)
            if firstOfTurn { queuedSteps.removeAll() }

        case "scope_closed":
            guard let id = fields["id"] as? UInt64 else { return }
            updateScope(id) { scope in
                scope.state = .idle
                scope.elapsed = fields["ms"] as? UInt64
            }

        // §12.9's ladder, as the reader sees it happening. Without these the search scope opens,
        // sits for several seconds with nothing in it, and closes.
        case "searched":
            guard let query = fields["query"] as? String else { return }
            appendSearchStep(SearchStep(kind: .searching(query: query)))

        case "fetched":
            guard let url = fields["url"] as? String else { return }
            let host = URL(string: url)?.host()?.replacingOccurrences(of: "www.", with: "") ?? url
            let verdict = fields["verdict"] as? String ?? ""
            // A page that was reached and not read is what the ladder climbed for, so it is worth
            // a row of its own rather than being folded into the one above it.
            appendSearchStep(
                SearchStep(
                    kind: verdict == "ok"
                        ? .reading(host: host)
                        : .rung(host: host, number: rungNumber(fields), verdict: verdict),
                    done: true
                )
            )

        case "tool_called":
            guard let tool = fields["tool"] as? String else { return }
            if fields["tier"] as? String == "irreversible" { composer = .needsYou }
            appendStep(Step(verb: "call", detail: tool, output: fields["args"] as? String))

        case "tool_returned":
            // The output lands on the step that called it, so the well sits under its own row
            // rather than at the end of the scope.
            let summary = fields["summary"] as? String
            attachOutput(summary)

        // Lane 2's steps, as they happen. Without these the memory scope opens, sits for as long
        // as a model call per step takes, and closes with nothing having been said.
        case "memory_consulted":
            guard let step = fields["step"] as? String else { return }
            appendSearchStep(
                SearchStep(
                    kind: .consulting(step: step, found: fields["found"] as? Bool ?? false),
                    done: true
                ),
                to: "memory"
            )

        case "memory_recalled":
            // `claim_ids`, and each one is an object rather than a string. Reading the wrong key
            // made every turn say it recalled nothing, whatever it had actually found (B-46).
            let claims = (fields["claim_ids"] as? [[String: Any]]) ?? []
            let count = claims.count
            // **What it recalled, not just how many.** "3 facts" says nothing a reader can check;
            // the concepts behind them are the part worth showing, and they are already in the
            // event.
            let from = Set(
                claims.compactMap { claim -> String? in
                    guard let concept = claim["concept"] as? String else { return nil }
                    return concept.split(separator: "/").last.map {
                        $0.replacingOccurrences(of: ".md", with: "")
                    }
                }
            ).sorted()
            // Lane 2 returns file lines rather than addressed claims, so it never has ids to
            // count. Saying "0 facts" for a search that found plenty would be worse than silence.
            if fields["lane"] as? String == "deliberate" {
                // The steps themselves are already on the memory trace. Repeating "search memory"
                // as a thinking step said less than the rows above it and read as the whole of what
                // lane 2 had done.
                return
            } else {
                let what = count == 1 ? "1 fact" : "\(count) facts"
                appendStep(
                    Step(
                        verb: "recall",
                        detail: from.isEmpty ? what : "\(what) from \(from.joined(separator: ", "))"
                    )
                )
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
            attachSources()
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

    /// Puts the turn's sources on the answer that used them (§12.7).
    ///
    /// Read after the turn, like the recall rail, because what an answer rested on is only settled
    /// once it has been written. Attached to the last assistant turn rather than held beside the
    /// thread, so scrolling back to an old answer still shows what that one cited.
    private func attachSources() {
        let sources = (core?.cited ?? []).map(Source.init)
        guard !sources.isEmpty else { return }
        guard let index = entries.lastIndex(where: {
            if case .turn(let turn) = $0 { return turn.speaker == .assistant }
            return false
        }), case .turn(var turn) = entries[index] else { return }
        turn.sources = sources
        withAnimation(Theme.Motion.arrive) { entries[index] = .turn(turn) }
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

/// Which trace is still counting, and when a turn starts and stops.
///
/// **A struct, so the two-turn sequence can be replayed in a test.** The rule reads simply and the
/// defect was never in the rule: `newestScope` kept pointing at the finished turn's scope, so the
/// instant the next turn set `working`, the old trace was live again for one tick and recomputed
/// its age from a start a whole turn earlier (B-75).
struct TurnLife: Equatable {
    private(set) var working = false
    private(set) var began: ContinuousClock.Instant?
    private(set) var newestScope: UInt64?

    /// Idempotent within a turn: the reader started waiting when they pressed send, not when the
    /// core got round to saying so.
    mutating func begin(at now: ContinuousClock.Instant = .now) {
        if !working { began = now }
        working = true
        newestScope = nil
    }

    mutating func opened(_ scope: UInt64) {
        newestScope = scope
    }

    /// The answer is fully on screen. Only the streamer decides this.
    mutating func end() {
        working = false
        began = nil
    }

    /// A scope of the running turn stays live until the answer has finished painting, so the
    /// figure the reader is left with is the wait they actually had rather than the model call
    /// inside it.
    func isLive(scope id: UInt64, closed: Bool) -> Bool {
        if !closed { return true }
        guard working else { return false }
        return newestScope == id
    }
}
