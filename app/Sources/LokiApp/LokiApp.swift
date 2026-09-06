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
            // The mark, not a generic glyph. Rendered rather than templated, because the menu
            // bar tints a template image to match the bar and the whole point of this one is that
            // it is yellow.
            // The raw file reports itself as 900 points, so handing it straight to the menu bar
            // asked for a logo the height of the screen. This one is rendered at 18.
            if let icon = Brand.mark(points: 18) {
                Image(nsImage: icon)
            } else {
                Image(systemName: "circle.fill")
            }
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
        // The Dock icon, set here rather than left to `CFBundleIconFile`. That key only applies to
        // an assembled `.app`, and running the SwiftPM executable from Xcode has no bundle to read
        // it from, so the Dock shows the generic executable icon instead of the mark.
        // **Three steps, and the last two are why this kept not working.** Setting the image is
        // not enough for a bare SwiftPM executable: it has no bundle, so it starts as an accessory
        // with no Dock tile to put an icon on, and a tile that already exists does not repaint
        // itself when the image behind it changes. Xcode runs exactly that bare executable, which
        // is why an assembled `Loki.app` looked correct the whole time (B-71).
        NSApp.setActivationPolicy(.regular)
        Brand.applyToDock()
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
    /// and a delegate that has already returned cannot hold the process open for them.
    ///
    /// **The bound is in the core, not here** (B-48). This used to start a second task that called
    /// `done.cancel()` after twenty seconds, which looked like a timeout and was not: the work is
    /// one blocking call across the FFI, and cancelling a Swift task cannot interrupt a blocking
    /// call. So cmd-Q waited for however long consolidation took, and stopped working altogether
    /// as the pass grew. `loki_end_session` now bounds itself around its own await points, which
    /// is the only place the bound can actually hold.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !closing else { return .terminateNow }
        closing = true

        Task { @MainActor in
            await conversation.endSession()
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

