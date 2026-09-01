import SwiftUI

/// The menu bar popover.
///
/// Carries one bit of state and never a count. A badge would need background work, which
/// principle 8 forbids.
struct MenuBarView: View {
    let conversation: Conversation
    let onOpen: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            header

            if let error = conversation.lastError, !conversation.isReady {
                Text(error)
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.muted)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                summary
            }

            Divider().overlay(Theme.Colors.line)

            Button("Open Loki") {
                uiTrace("4 Open Loki button fired")
                onOpen()
            }
                .buttonStyle(.borderless)
                .font(Theme.Text.body)

            Button("Quit") { NSApplication.shared.terminate(nil) }
                .buttonStyle(.borderless)
                .font(Theme.Text.body)
                .keyboardShortcut("q")
        }
        .padding(Theme.Space.l)
        .frame(width: 300, alignment: .leading)
        .background(Theme.Colors.raised)
    }

    private var header: some View {
        HStack {
            Image(systemName: state.glyph)
                .font(.system(size: 9))
                .foregroundStyle(state.color)
            Text("Loki")
                .font(Theme.Text.title)
                .kerning(Theme.Text.titleTracking)
                .foregroundStyle(Theme.Colors.ink)
            Spacer()
            Text(conversation.isReady ? state.label : "no key")
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .foregroundStyle(Theme.Colors.faint)
        }
    }

    /// The session summary. Silent when nothing happened, because a card that says
    /// "learned nothing today" teaches people to ignore the card.
    @ViewBuilder
    private var summary: some View {
        if conversation.turns.isEmpty {
            Text("Nothing yet today.")
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.faint)
        } else {
            VStack(alignment: .leading, spacing: Theme.Space.s) {
                Text("This session")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.faint)
                Text("\(conversation.turns.count) turns.")
                    .font(Theme.Text.record)
                    .foregroundStyle(Theme.Colors.ink)
            }
            .padding(Theme.Space.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.panel))
        }
    }

    private var state: Theme.State {
        switch conversation.composer {
        case .running: .holding
        case .needsYou: .needsYou
        default: .released
        }
    }
}
