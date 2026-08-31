import LokiCore
import SwiftUI

/// Menu bar app. `LSUIElement` in the bundle Info.plist keeps it out of the Dock.
@main
struct LokiApp: App {
    @State private var conversation = Conversation()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView(conversation: conversation)
        } label: {
            Image(systemName: "square.fill")
        }
        .menuBarExtraStyle(.window)

        Window("Loki", id: "thread") {
            ThreadWindow(conversation: conversation)
        }
        .defaultSize(width: 980, height: 720)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Interrupt") { conversation.interrupt() }
                    .keyboardShortcut(.escape, modifiers: [])
            }
        }
    }
}

private struct ThreadWindow: View {
    let conversation: Conversation

    var body: some View {
        VStack(spacing: 0) {
            TopBar(conversation: conversation)
            Divider().overlay(Theme.Colors.line)
            ThreadView(conversation: conversation)
            Composer(conversation: conversation)
        }
        .background(Theme.Colors.canvas)
        .frame(minWidth: 620, minHeight: 420)
        .task { conversation.observe() }
    }
}

private struct TopBar: View {
    let conversation: Conversation

    var body: some View {
        HStack(spacing: Theme.Space.m) {
            Text("Loki")
                .font(Theme.Text.title)
                .kerning(Theme.Text.titleTracking)
                .foregroundStyle(Theme.Colors.ink)
            Spacer()
            Text("core \(Core.version)")
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.faint)
        }
        .padding(.horizontal, Theme.Space.l)
        .padding(.vertical, Theme.Space.m)
        .background(.regularMaterial)
    }
}
