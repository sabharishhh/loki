import SwiftUI

/// What Loki did while it was working, and how long it took.
///
/// **This replaces the released state.** A rail that turned green when a step finished told you
/// something had ended and nothing about what it was. The useful facts are what it did and how
/// long you waited, so those are what this says.
///
/// **A line, not a panel.** It was a full width strip with a border and a filled ground, which is
/// a lot of furniture for one sentence and made every answer look like it came with an attachment.
/// The line hugs its own text and the only feedback on hover is the text lifting a shade. Steps
/// open underneath it with no container of their own.
///
/// It opens itself while the model works and closes when it stops, which is the behaviour worth
/// copying from the tools that do this well: you see the work happening without being asked to
/// care, and it gets out of the way once there is an answer to read. Opening it again is one
/// click and it stays open, because a block that keeps re-collapsing under you is infuriating.
struct ThinkingTrace: View {
    let scope: Scope
    /// Still running. Drives the timer, the auto-open and the pulse on the icon.
    let live: Bool

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var open = false
    /// Set once the reader has expressed a preference, after which nothing opens or closes it.
    @State private var pinned = false
    @State private var elapsed: Duration = .zero
    /// Whether this view watched the wait rather than arriving after it. A trace that counted its
    /// own seconds keeps them; one rendered from a finished event takes the figure it was given.
    @State private var counted = false
    @State private var hovering = false

    /// Nothing to open. A chevron that discloses an empty box is worse than no chevron.
    private var hasSteps: Bool { !scope.steps.isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            if hasSteps || live {
                header
            } else {
                label
            }
            if open, hasSteps || live {
                steps
                    .transition(
                        .asymmetric(
                            insertion: .push(from: .top).combined(with: .opacity),
                            removal: .opacity
                        )
                    )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(Theme.Motion.disclose, value: open)
        .onChange(of: live, initial: true) { _, running in
            guard !pinned else { return }
            open = running
        }
        .task(id: live) { await tick() }
    }

    private var header: some View {
        Button {
            pinned = true
            open.toggle()
        } label: {
            HStack(spacing: Theme.Space.xs) {
                label
                Image(systemName: "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(Theme.Colors.tertiary)
                    .rotationEffect(.degrees(open ? 0 : -90))
                    .animation(Theme.Motion.control, value: open)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .accessibilityLabel(caption)
        .accessibilityHint(open ? "Hides the working steps" : "Shows the working steps")
    }

    /// The icon and the sentence. The whole of the trace when there is nothing to disclose.
    private var label: some View {
        HStack(spacing: Theme.Space.s) {
            // A trace, not the mark. The mark means Loki is speaking, and this line is the working
            // underneath rather than the answer, so it stays in the neutral ramp. Deliberately not
            // a sparkle or a lightbulb, which are the two icons every AI interface reaches for.
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(live ? Theme.Colors.secondary : Theme.Colors.tertiary)
                .symbolEffect(.pulse, isActive: live && !reduceMotion)
            Text(caption)
                .font(Theme.Text.body)
                .foregroundStyle(tint)
        }
    }

    /// One shade, and only while the pointer is on it. Enough to say the line answers to a click.
    private var tint: Color {
        if live { return Theme.Colors.primary }
        return hovering ? Theme.Colors.primary : Theme.Colors.secondary
    }

    /// "Thinking for 4s" while it runs, "Thought for 4s" once it has stopped.
    ///
    /// Seconds, never milliseconds. A figure to three decimal places is precision nobody asked
    /// for and it reads as machine output rather than as an answer to how long you waited.
    private var caption: String {
        // Rounded, not truncated. A wait of 1.9 seconds reported as "1s" is the interface
        // disagreeing with the reader about something they just sat through.
        let seconds = Int((Double(elapsed.components.seconds)
            + Double(elapsed.components.attoseconds) / 1e18).rounded())
        if live {
            return seconds < 1 ? "Thinking" : "Thinking for \(seconds)s"
        }
        guard seconds >= 1 else { return "Thought for a moment" }
        return "Thought for \(seconds)s"
    }

    private var steps: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            ForEach(scope.steps) { step in
                HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s) {
                    Circle()
                        .fill(Theme.Colors.tertiary)
                        .frame(width: 3, height: 3)
                        .offset(y: -3)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(step.verb)
                            .font(Theme.Text.body)
                            .foregroundStyle(Theme.Colors.secondary)
                        if let output = step.output, !output.isEmpty {
                            Text(output)
                                .font(Theme.Text.meta)
                                .foregroundStyle(Theme.Colors.tertiary)
                                .lineLimit(3)
                        }
                    }
                }
                .transition(.opacity.combined(with: .offset(y: 4)))
            }
            if scope.steps.isEmpty {
                Text("Working on it.")
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.tertiary)
            }
        }
        // Aligned under the sentence rather than under the icon, so the steps read as belonging
        // to it. Left inset only: nothing draws a box around them.
        .padding(.leading, Theme.Space.l)
        .frame(maxWidth: .infinity, alignment: .leading)
        .animation(Theme.Motion.arrive, value: scope.steps.count)
    }

    /// Counts while the turn is live, then holds the figure.
    ///
    /// **The wait, not the model call.** `ScopeClosed` reports how long the provider took. That
    /// leaves out everything on either side of it: recall, which runs before the call and opens no
    /// scope of its own, and the drain, which goes on painting after the last token has landed. So
    /// the clock runs from when the reader pressed send to when the answer stopped moving, and the
    /// event's figure is used only for a trace rendered after the fact (B-61).
    private func tick() async {
        if !counted, let ms = scope.elapsed {
            elapsed = .milliseconds(ms)
        }
        guard live else { return }
        counted = true
        while !Task.isCancelled {
            elapsed = ContinuousClock.now - scope.began
            try? await Task.sleep(for: .milliseconds(reduceMotion ? 1000 : 120))
        }
    }
}

#Preview("Working") {
    VStack(alignment: .leading, spacing: 16) {
        ThinkingTrace(
            scope: Scope(
                id: 1,
                kind: "model",
                state: .thinking,
                steps: [
                    Step(verb: "Searching memory for what you said about Meera", detail: ""),
                    Step(verb: "Reading people/meera.md", detail: "", output: "Works on the infra team"),
                ]
            ),
            live: true
        )
        ThinkingTrace(
            scope: Scope(id: 2, kind: "model", state: .idle, steps: [], elapsed: 4200),
            live: false
        )
    }
    .padding(24)
    .frame(width: 560)
    .background(Theme.Colors.background)
}
