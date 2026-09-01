import SwiftUI

/// The thread.
///
/// The user's turn sits right in an inverted box and the assistant's runs full measure with no
/// container. Two sides, one spine: only the instruction is boxed, so the record still reads as
/// prose rather than as a chat log.
struct ThreadView: View {
    let conversation: Conversation

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.Space.xl) {
                    ForEach(conversation.entries) { entry in
                        switch entry {
                        case .turn(let turn): TurnView(turn: turn)
                        case .scope(let scope): ScopeRail(scope: scope)
                        }
                    }

                    if let error = conversation.lastError {
                        ErrorRow(message: error)
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
            .onChange(of: conversation.entries.last?.id) {
                guard let last = conversation.entries.last else { return }
                withAnimation(Theme.Motion.standard) { proxy.scrollTo(last.id, anchor: .bottom) }
            }
        }
        .background(Theme.Colors.raised)
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
                    .foregroundStyle(Theme.Colors.onInverted)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, Theme.Space.l)
                    .padding(.vertical, Theme.Space.m)
                    .background(
                        Theme.Colors.inverted,
                        in: .rect(cornerRadius: Theme.Radius.bubble)
                    )
            }

        case .assistant:
            // Full measure, no container. Prose, because it is prose.
            Text(turn.text)
                .font(Theme.Text.record)
                .lineSpacing(Theme.Text.recordLineSpacing)
                .foregroundStyle(Theme.Colors.ink)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
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
                .foregroundStyle(Theme.Colors.ink)
        }
        .padding(Theme.Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.State.needsYou.tint, in: .rect(cornerRadius: Theme.Radius.control))
    }
}
