import SwiftUI

/// Text that reads as being worked on.
///
/// A band of light travels along the glyphs, left to right, and the text under it is ordinary text
/// the whole time. **Not a skeleton and not a spinner:** it says which words are provisional
/// without replacing them, so nothing moves when the answer arrives and there is no widget whose
/// only job is to say the machine is busy.
struct Shimmer: View {
    let text: String
    var font: Font = Theme.Text.body

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var travel: CGFloat = -1

    var body: some View {
        Text(text)
            .font(font)
            .foregroundStyle(Theme.Colors.secondary)
            .overlay {
                if !reduceMotion {
                    // The highlight is drawn over the words and clipped to them, so it lights the
                    // glyphs rather than sweeping a rectangle across the line.
                    LinearGradient(
                        colors: [.clear, Theme.Colors.primary, .clear],
                        startPoint: .leading,
                        endPoint: .trailing
                    )
                    .frame(width: 90)
                    .offset(x: travel * 160)
                    .mask(Text(text).font(font))
                    .allowsHitTesting(false)
                }
            }
            .task(id: reduceMotion) {
                guard !reduceMotion else { return }
                // A slow sweep with a pause at the end, so it reads as attention moving along a
                // line rather than as a barber's pole.
                while !Task.isCancelled {
                    withAnimation(.easeInOut(duration: 1.5)) { travel = 1 }
                    try? await Task.sleep(for: .milliseconds(1700))
                    travel = -1
                    try? await Task.sleep(for: .milliseconds(300))
                }
            }
            .accessibilityLabel(text)
    }
}

/// One thing a search did, on the web or through memory.
struct SearchStep: Identifiable, Equatable {
    enum Kind: Equatable {
        case searching(query: String)
        case reading(host: String)
        /// A page the ladder reached and could not read, and how far it got.
        ///
        /// **The host is the point.** These arrived as "Rung 1 said blocked" with nothing naming
        /// the page, one row per rung, so three unreadable pages read as six identical lines of
        /// noise and said nothing about what had actually happened.
        case rung(host: String, number: Int, verdict: String)
        /// One step of a memory search, as the navigator asked for it.
        case consulting(step: String, found: Bool)

        var glyph: String {
            switch self {
            case .searching: "magnifyingglass"
            case .reading: "doc.text"
            case .rung: "arrow.turn.up.right"
            case .consulting: "brain"
            }
        }

        /// The host a page step is about, so two rungs on one page can share a row.
        var host: String? {
            switch self {
            case .reading(let host), .rung(let host, _, _): host
            default: nil
            }
        }

        var sentence: String {
            switch self {
            case .searching(let query): "Searching for \(query)"
            case .reading(let host): "Reading \(host)"
            case .rung(let host, let number, let verdict):
                "\(host): \(Self.plainly(verdict)) at rung \(number)"
            case .consulting(let step, let found):
                found ? step : "\(step), nothing there"
            }
        }

        /// The core's verdict vocabulary in words a reader has met before (§12.2).
        static func plainly(_ verdict: String) -> String {
            switch verdict {
            case "js_required": "needs JavaScript"
            case "rate_limited": "rate limited"
            case "not_found": "not found"
            case "interaction_required": "needs a click"
            case "exhausted": "nothing readable"
            default: verdict
            }
        }
    }

    let id = UUID()
    let kind: Kind
    var done = false
}

extension Array where Element == SearchStep {
    /// Adds a step, and marks everything before it finished.
    ///
    /// **One row per page, not one per rung.** The ladder tries each rung in turn and reports every
    /// attempt, so an unreadable page wrote "Rung 1 said blocked" and then "Rung 2 said blocked"
    /// and three of them filled the trace with six lines that named nothing. A further attempt on
    /// the same page replaces the row the last one left, so the row says how far the ladder got
    /// rather than how many times it tried.
    ///
    /// Pulled out of the conversation so it can be tested without a running core.
    mutating func advance(with step: SearchStep) {
        for at in indices { self[at].done = true }
        if case .rung = step.kind, let host = step.kind.host, let last = indices.last,
            case .rung = self[last].kind, self[last].kind.host == host
        {
            self[last] = step
        } else {
            append(step)
        }
    }
}

/// What a web search did, while it is doing it.
///
/// **It stays after the answer arrives.** The tools this borrows from make the trace vanish the
/// moment output begins, which is defensible for a chat and wrong here: §12.7 requires an answer to
/// carry where it came from, and a trace that deletes itself takes the reasoning with it. So it
/// collapses to one line instead, and the line is still a control.
///
/// Live, the current step shimmers and the finished ones are stated plainly. That is the whole
/// distinction the interface has to carry: what is happening now, and what already did.
struct SearchTrace: View {
    /// Which retrieval this is. The surface is the same; only the words and the icon differ.
    ///
    /// **Memory used to get a bare step list and the web got this**, so the one retrieval that runs
    /// before the answer with a model call per step was also the one that showed nothing while it
    /// ran.
    enum Subject {
        case web
        case memory

        var glyph: String {
            switch self {
            case .web: "globe"
            case .memory: "brain"
            }
        }

        var whileWaiting: String {
            switch self {
            case .web: "Searching the web"
            case .memory: "Looking through my memory"
            }
        }
    }

    let steps: [SearchStep]
    let live: Bool
    var subject: Subject = .web
    var elapsed: Duration = .zero

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var open = false
    /// Counted here while the search runs.
    ///
    /// **The scope's own figure only exists once it has closed.** `elapsed` arrives with
    /// `ScopeClosed`, so a trace that showed only that had nothing to say for the several seconds
    /// the reader is actually waiting, which is the whole time it matters.
    @State private var ticked: Duration = .zero
    @State private var counted = false

    private var current: SearchStep? { steps.last(where: { !$0.done }) }

    /// What to show: the live count while it runs, the scope's figure once it has stopped.
    private var showing: Duration { counted ? ticked : elapsed }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            header
            if open {
                ForEach(steps) { step in
                    row(step)
                        .transition(.opacity.combined(with: .offset(y: -4)))
                }
                .padding(.leading, Theme.Space.l)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(reduceMotion ? nil : Theme.Motion.disclose, value: open)
        .animation(reduceMotion ? nil : Theme.Motion.arrive, value: steps.count)
        // The line stops shimmering and starts stating what it cost. Without this the two swap in
        // one frame at the moment the reader is looking straight at it.
        .animation(reduceMotion ? nil : Theme.Motion.control, value: live)
        .task(id: live) { await tick() }
    }

    private var header: some View {
        Button { open.toggle() } label: {
            HStack(spacing: Theme.Space.s) {
                Image(systemName: subject.glyph)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(live ? Theme.Colors.yellow : Theme.Colors.tertiary)
                    .symbolEffect(.pulse, isActive: live && !reduceMotion)

                // While it runs, the line says what is happening now. Afterwards it says what it
                // cost, because that is the fact worth keeping.
                //
                // **A search reports each piece after it is done**, so for the first few seconds
                // there is no step to name. Saying so beats falling through to a summary that
                // reads "0 pages" while the work is still going on.
                if live {
                    Shimmer(text: current?.kind.sentence ?? subject.whileWaiting)
                        .transition(.opacity)
                    Text(waited)
                        .font(Theme.Text.micro)
                        .foregroundStyle(Theme.Colors.tertiary)
                        .monospacedDigit()
                        .contentTransition(.numericText())
                } else {
                    Text(summary)
                        .font(Theme.Text.body)
                        .foregroundStyle(Theme.Colors.secondary)
                        .monospacedDigit()
                        .contentTransition(.numericText())
                }

                Image(systemName: "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(Theme.Colors.tertiary)
                    .rotationEffect(.degrees(open ? 0 : -90))
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(summary)
    }

    /// The running figure, for the line that is still counting.
    private var waited: String {
        let seconds = Int(
            (Double(showing.components.seconds)
                + Double(showing.components.attoseconds) / 1e18).rounded())
        return seconds >= 1 ? "\(seconds)s" : ""
    }

    /// Counts while the search runs, then holds what it reached.
    private func tick() async {
        guard live else { return }
        counted = true
        let began = ContinuousClock.now
        while !Task.isCancelled {
            ticked = ContinuousClock.now - began
            try? await Task.sleep(for: .milliseconds(reduceMotion ? 1000 : 120))
        }
    }

    private var summary: String {
        let seconds = Int(
            (Double(showing.components.seconds)
                + Double(showing.components.attoseconds) / 1e18).rounded())
        let opened: String
        switch subject {
        case .web:
            let read = steps.filter { if case .reading = $0.kind { true } else { false } }.count
            opened = "Searched the web, " + (read == 1 ? "1 page" : "\(read) pages")
        case .memory:
            let looked = steps.filter { if case .consulting = $0.kind { true } else { false } }.count
            opened = "Searched my memory, " + (looked == 1 ? "1 step" : "\(looked) steps")
        }
        return seconds >= 1 ? "\(opened), \(seconds)s" : opened
    }

    @ViewBuilder
    private func row(_ step: SearchStep) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s) {
            Image(systemName: step.done ? "checkmark" : step.kind.glyph)
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(step.done ? Theme.Colors.tertiary : Theme.Colors.yellow)
                .frame(width: 12)
            if step.done {
                Text(step.kind.sentence)
                    .font(Theme.Text.meta)
                    .foregroundStyle(Theme.Colors.tertiary)
            } else {
                Shimmer(text: step.kind.sentence, font: Theme.Text.meta)
            }
            Spacer(minLength: 0)
        }
    }
}

#Preview("Searching") {
    VStack(alignment: .leading, spacing: 28) {
        SearchTrace(
            steps: [
                SearchStep(kind: .searching(query: "loki memory architecture"), done: true),
                SearchStep(kind: .reading(host: "example.com"), done: true),
                SearchStep(kind: .reading(host: "en.wikipedia.org")),
            ],
            live: true
        )
        SearchTrace(
            steps: [
                SearchStep(kind: .searching(query: "rust tls impersonation"), done: true),
                SearchStep(kind: .reading(host: "github.com"), done: true),
            ],
            live: false,
            elapsed: .seconds(4)
        )
        Shimmer(text: "Reading three sources")
    }
    .padding(28)
    .frame(width: 560)
    .background(Theme.Colors.background)
}
