import AppKit
import Foundation
import LokiCore
import SwiftUI

/// Menu bar app. `LSUIElement` in the bundle Info.plist keeps it out of the Dock.
@main
struct LokiApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        MenuBarExtra {
            let _ = uiTrace("2 MenuBarExtra content built")
            MenuBarView(conversation: delegate.conversation, onOpen: delegate.openThread)
        } label: {
            Image(systemName: "square.fill")
        }
        .menuBarExtraStyle(.window)
    }
}

/// Owns everything that outlives a window.
///
/// An `NSApplicationDelegate` rather than state on the `App`, because the window has to be
/// created after the run loop is up. A task scheduled from an initializer never gets there.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let conversation = Conversation()
    private var thread: ThreadWindowController?

    override init() {
        super.init()
        uiTrace("1 AppDelegate.init")
    }

    /// The thread is the product. It opens on launch rather than waiting to be found in the
    /// menu bar, which is a shortcut back to it, not the way in.
    func applicationDidFinishLaunching(_ notification: Notification) {
        uiTrace("3 didFinishLaunching policy=\(NSApp.activationPolicy().rawValue)")
        openThread()
    }

    /// Reopening from Finder or the Dock shows the thread rather than doing nothing.
    ///
    /// Returns false because this handled it. Returning true lets AppKit also run its own reopen,
    /// which creates a stray untitled window beside ours.
    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows: Bool
    ) -> Bool {
        openThread()
        return false
    }

    func openThread() {
        uiTrace("5 AppDelegate.openThread")
        let thread = thread ?? ThreadWindowController(conversation: conversation)
        self.thread = thread
        thread.show()
    }
}

struct ThreadWindow: View {
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
        // Leave room for the traffic lights, since the titlebar is transparent.
        .padding(.leading, 78)
        .padding(.trailing, Theme.Space.l)
        .padding(.vertical, Theme.Space.m)
        .background(.regularMaterial)
    }
}
