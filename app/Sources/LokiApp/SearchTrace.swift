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

/// One thing the search did.
struct SearchStep: Identifiable, Equatable {
    enum Kind: Equatable {
        case searching(query: String)
        case reading(host: String)
        case rung(number: Int, verdict: String)

        var glyph: String {
            switch self {
            case .searching: "magnifyingglass"
            case .reading: "doc.text"
            case .rung: "arrow.turn.up.right"
            }
        }

        var sentence: String {
            switch self {
            case .searching(let query): "Searching for \(query)"
            case .reading(let host): "Reading \(host)"
            case .rung(let number, let verdict): "Rung \(number) said \(verdict)"
            }
        }
    }

    let id = UUID()
    let kind: Kind
    var done = false
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
    let steps: [SearchStep]
    let live: Bool
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
                Image(systemName: "globe")
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
                    Shimmer(text: current?.kind.sentence ?? "Searching the web")
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
        let read = steps.filter { if case .reading = $0.kind { true } else { false } }.count
        let seconds = Int(
            (Double(showing.components.seconds)
                + Double(showing.components.attoseconds) / 1e18).rounded())
        let pages = read == 1 ? "1 page" : "\(read) pages"
        return seconds >= 1 ? "Searched the web, \(pages), \(seconds)s" : "Searched the web, \(pages)"
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
