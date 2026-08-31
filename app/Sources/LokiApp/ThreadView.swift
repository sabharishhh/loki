import SwiftUI

/// The thread.
///
/// No bubbles. The mental model is supervision, not conversation, and a bubble would give the eye
/// a second vertical spine competing with the rail.
struct ThreadView: View {
    let conversation: Conversation

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.Space.xl) {
                    ForEach(conversation.turns) { turn in
                        TurnView(turn: turn).id(turn.id)
                    }

                    ForEach(conversation.scopes) { scope in
                        ScopeRail(scope: scope).id(scope.id)
                    }

                    if let error = conversation.lastError {
                        ErrorRow(message: error)
                    }
                }
                .frame(maxWidth: Theme.Size.measure, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, Theme.Space.xl)
                .padding(.vertical, Theme.Space.xxl)
            }
            .onChange(of: conversation.turns.last?.text) {
                guard let last = conversation.turns.last else { return }
                withAnimation(Theme.Motion.standard) { proxy.scrollTo(last.id, anchor: .bottom) }
            }
        }
        .background(Theme.Colors.raised)
    }
}

private struct TurnView: View {
    let turn: Turn

    var body: some View {
        switch turn.speaker {
        case .user:
            // Indented behind a rule, reading as a quoted instruction.
            HStack(alignment: .top, spacing: Theme.Space.m) {
                Rectangle()
                    .fill(Theme.Colors.line)
                    .frame(width: Theme.Size.rail)
                Text(turn.text)
                    .font(Theme.Text.body)
                    .lineSpacing(Theme.Text.bodyLineSpacing)
                    .foregroundStyle(Theme.Colors.muted)
                    .textSelection(.enabled)
            }
            .fixedSize(horizontal: false, vertical: true)

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
