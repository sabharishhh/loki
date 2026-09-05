import SwiftUI

/// What Loki did while it was working, and how long it took.
///
/// **This replaces the released state.** A rail that turned green when a step finished told you
/// something had ended and nothing about what it was. The useful facts are what it did and how
/// long you waited, so those are what this says.
///
/// It opens itself while the model works and closes when it stops, which is the behaviour worth
/// copying from the tools that do this well: you see the work happening without being asked to
/// care, and it gets out of the way once there is an answer to read. Opening it again is one
/// click and it stays open, because a block that keeps re-collapsing under you is infuriating.
struct ThinkingTrace: View {
    let scope: Scope
    /// Still running. Drives the timer, the auto-open and the shimmer on the label.
    let live: Bool

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var open = false
    /// Set once the reader has expressed a preference, after which nothing opens or closes it.
    @State private var pinned = false
    @State private var elapsed: Duration = .zero
    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if open {
                steps
                    .transition(
                        .asymmetric(
                            insertion: .push(from: .top).combined(with: .opacity),
                            removal: .opacity
                        )
                    )
            }
        }
        .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.panel))
        .overlay {
            RoundedRectangle(cornerRadius: Theme.Radius.panel)
                .strokeBorder(open ? Theme.Colors.borderStrong : Theme.Colors.border, lineWidth: 1)
        }
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
            HStack(spacing: Theme.Space.s) {
                MarkBadge(state: live ? .thinking : .idle, size: 15, animated: live)
                Text(caption)
                    .font(Theme.Text.body)
                    .foregroundStyle(live ? Theme.Colors.primary : Theme.Colors.secondary)
                Spacer(minLength: Theme.Space.s)
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.Colors.tertiary)
                    .rotationEffect(.degrees(open ? 0 : -90))
                    .animation(Theme.Motion.control, value: open)
            }
            .padding(.horizontal, Theme.Space.m)
            .padding(.vertical, Theme.Space.s + 1)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .background(hovering ? Theme.Colors.surfaceAlt : .clear)
        .clipShape(.rect(cornerRadius: Theme.Radius.panel))
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .accessibilityLabel(caption)
        .accessibilityHint(open ? "Hides the working steps" : "Shows the working steps")
    }

    /// "Thinking for 4s" while it runs, "Thought for 4s" once it has stopped.
    ///
    /// Seconds, never milliseconds. A figure to three decimal places is precision nobody asked
    /// for and it reads as machine output rather than as an answer to how long you waited.
    private var caption: String {
        let seconds = max(elapsed.components.seconds, 0)
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
                        .frame(width: 4, height: 4)
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
                Text(live ? "Working on it." : "Nothing worth showing.")
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.tertiary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Theme.Space.m)
        .padding(.bottom, Theme.Space.m)
        .animation(Theme.Motion.arrive, value: scope.steps.count)
    }

    /// Counts while the scope is live, then holds the final figure.
    ///
    /// Driven by a clock rather than by a stored start date so a trace rendered after the fact,
    /// from an event that already carries its duration, shows the real number rather than zero.
    private func tick() async {
        if let ms = scope.elapsed {
            elapsed = .milliseconds(ms)
        }
        guard live else { return }
        let started = ContinuousClock.now
        while !Task.isCancelled {
            elapsed = ContinuousClock.now - started
            try? await Task.sleep(for: .milliseconds(reduceMotion ? 1000 : 120))
        }
    }
}

#Preview("Working") {
    VStack(spacing: 16) {
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
