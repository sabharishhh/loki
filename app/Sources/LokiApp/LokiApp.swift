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
    /// Guards against re-entering the close when the reply comes back through the delegate.
    private var closing = false

    override init() {
        super.init()
        uiTrace("1 AppDelegate.init")
    }

    /// The thread is the product. It opens on launch rather than waiting to be found in the
    /// menu bar, which is a shortcut back to it, not the way in.
    func applicationDidFinishLaunching(_ notification: Notification) {
        uiTrace("3 didFinishLaunching policy=\(NSApp.activationPolicy().rawValue)")

        // A clash means another app already owns these keys. Not an error: the menu bar and the
        // window both still work, so carry on and say so in the trace.
        let claimed = GlobalHotkey.optionSpace.register { [weak self] in self?.openThread() }
        uiTrace("hotkey opt+space claimed=\(claimed)")

        openThread()
    }

    /// Consolidation runs at session close, because the app is already awake (§9.8).
    ///
    /// `applicationShouldTerminate` rather than `willTerminate`, since the pass makes model calls
    /// and a delegate that has already returned cannot hold the process open for them. Bounded, so
    /// a slow provider cannot make quitting feel broken: losing one session's consolidation costs
    /// a re-derivation, and the episode file is still on disk either way.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !closing else { return .terminateNow }
        closing = true

        Task { @MainActor in
            let done = Task { await conversation.endSession() }
            let bound = Task {
                try? await Task.sleep(for: .seconds(20))
                done.cancel()
            }
            _ = await done.value
            bound.cancel()
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        GlobalHotkey.optionSpace.unregister()
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
        Shell(conversation: conversation)
    }
}

