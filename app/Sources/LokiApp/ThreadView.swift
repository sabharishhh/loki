import SwiftUI

/// The thread.
///
/// The user's turn sits right in an inverted box and the assistant's runs full measure with no
/// container. Two sides, one spine: only the instruction is boxed, so the record still reads as
/// prose rather than as a chat log.
struct ThreadView: View {
    let conversation: Conversation

    /// Whether the newest turn is on screen. Only when it is not does the app follow the stream,
    /// because yanking the view down while somebody is reading back is worse than a stale scroll.
    @State private var atBottom = true

    var body: some View {
        if conversation.entries.isEmpty && conversation.lastError == nil {
            Opening()
                .transition(.opacity)
                .background(Theme.Colors.background)
        } else {
            thread
        }
    }

    private var thread: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.Space.xl) {
                    ForEach(conversation.entries) { entry in
                        switch entry {
                        case .turn(let turn):
                            MessageRow(
                                turn: turn,
                                streaming: conversation.isStreaming(turn),
                                onEdit: { text in
                                    conversation.resend(from: turn.id, text: text)
                                }
                            )
                            .id(turn.id)
                        case .scope(let scope):
                            ThinkingTrace(scope: scope, live: scope.state != .idle)
                                .id(scope.id)
                        }
                    }

                    if let error = conversation.lastError {
                        ErrorRow(message: error)
                    }

                    if !conversation.summary.isEmpty {
                        SessionSummary(lines: conversation.summary)
                    }
                }
                // Capped at the measure, padded, then centred, in that order. The same three
                // steps the composer takes, so the column's edges line up with the field's and
                // a right-set turn lands on the same rule as the send button.
                .frame(maxWidth: Theme.Size.measure)
                .padding(.horizontal, Theme.Space.xl)
                .padding(.vertical, Theme.Space.xxl)
                .frame(maxWidth: .infinity)
            }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                // Within a line of the end counts as being at the end, or the caret flickers on
                // during the last few pixels of an ordinary scroll.
                geometry.contentOffset.y + geometry.containerSize.height
                    >= geometry.contentSize.height - 24
            } action: { _, atBottom in
                self.atBottom = atBottom
            }
            .onChange(of: conversation.entries.last?.id) {
                guard let last = conversation.entries.last, atBottom else { return }
                withAnimation(Theme.Motion.control) { proxy.scrollTo(last.id, anchor: .bottom) }
            }
            .overlay(alignment: .bottom) {
                if !atBottom {
                    ScrollDown {
                        guard let last = conversation.entries.last else { return }
                        withAnimation(Theme.Motion.panel) {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                    .padding(.bottom, Theme.Space.l)
                    .transition(.opacity.combined(with: .offset(y: 8)))
                }
            }
            .animation(Theme.Motion.control, value: atBottom)
        }
        .background(Theme.Colors.background)
    }
}

private struct TurnView: View {
    /// How much of the column a user turn leaves clear on its left. Sets where a long one wraps.
    private static let userTurnGutter: CGFloat = 0.25

    let turn: Turn

    var body: some View {
        switch turn.speaker {
        case .user:
            // A leading spacer, not a trailing frame. `frame(maxWidth:alignment:)` expands to
            // its maximum and then aligns the text inside that box, so a short turn ended up a
            // whole cap-width short of the right edge. A spacer lets the box hug its text and
            // pushes it against the column edge, and its minimum is what makes a long turn wrap.
            HStack(spacing: 0) {
                Spacer(minLength: Theme.Size.measure * Self.userTurnGutter)
                Text(turn.text)
                    .font(Theme.Text.body)
                    .lineSpacing(Theme.Text.bodyLineSpacing)
                    .foregroundStyle(Theme.Colors.primary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, Theme.Space.l)
                    .padding(.vertical, Theme.Space.m)
                    .background(
                        Theme.Colors.surfaceAlt,
                        in: .rect(cornerRadius: Theme.Radius.bubble)
                    )
            }

        case .assistant:
            // Full measure, no container. Prose, because it is prose.
            MarkdownText(text: turn.text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct ErrorRow: View {
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Space.s) {
            Image(systemName: Theme.State.needsYou.glyph)
                .font(.system(size: 10))
                .foregroundStyle(Theme.State.needsYou.color)
            Text(message)
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.primary)
        }
        .padding(Theme.Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.State.needsYou.tint, in: .rect(cornerRadius: Theme.Radius.control))
    }
}

/// What was learned this session (§17.4).
///
/// Inline at the end of the thread, not a modal. Up to three lines, and it never appears at all
/// when nothing happened: a card that says "learned nothing today" teaches people to ignore the
/// card, which costs the differentiator its only daily showing.
struct SessionSummary: View {
    let lines: [String]

    @State private var shown = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Text("Learned today")
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)

            ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                Text(line)
                    .font(Theme.Text.body)
                    .lineSpacing(Theme.Text.bodyLineSpacing)
                    .foregroundStyle(Theme.Colors.primary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Theme.Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.panel))
        .overlay(alignment: .leading) {
            // The same rail the thread uses, so the card reads as part of the record rather than
            // as a notification arriving on top of it.
            Rectangle()
                .fill(Theme.Colors.border)
                .frame(width: Theme.Size.rail)
        }
        .clipShape(.rect(cornerRadius: Theme.Radius.panel))
        .opacity(shown ? 1 : 0)
        .offset(y: shown ? 0 : 8)
        .onAppear { withAnimation(Theme.Motion.panel) { shown = true } }
    }
}

/// The way back to the newest turn.
///
/// Appears only when the end is off screen. A control that is always there is a control nobody
/// reads, and this one has to be noticed the one time it matters.
private struct ScrollDown: View {
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: "arrow.down")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(hovering ? Theme.Colors.onYellow : Theme.Colors.secondary)
                .frame(width: 26, height: 26)
                .background(
                    hovering ? Theme.Colors.yellow : Theme.Colors.surface,
                    in: .circle
                )
                .overlay {
                    Circle().strokeBorder(
                        hovering ? .clear : Theme.Colors.border,
                        lineWidth: 1
                    )
                }
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .help("Jump to the newest")
        .accessibilityLabel("Jump to the newest message")
    }
}
