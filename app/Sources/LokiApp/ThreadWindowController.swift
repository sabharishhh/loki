import AppKit
import SwiftUI

/// Owns the thread window.
///
/// AppKit rather than a SwiftUI `Window` scene. A scene in an `LSUIElement` app either opens at
/// launch behind everything, or is suppressed and then will not open from the menu bar, and which
/// of those happens differs between a bare binary and a bundle. Creating the window here removes
/// the ambiguity: it exists when asked for and never before.
@MainActor
final class ThreadWindowController: NSObject, NSWindowDelegate {
    private let conversation: Conversation
    private var window: NSWindow?

    init(conversation: Conversation) {
        self.conversation = conversation
    }

    var isOpen: Bool { window?.isVisible ?? false }

    /// Shows the window, creating it the first time. Brings it forward either way.
    func show() {
        // An accessory app is never the active application on its own, so without this the
        // window is ordered in behind whatever the user was looking at.
        NSApp.activate(ignoringOtherApps: true)

        if let window {
            window.makeKeyAndOrderFront(nil)
            return
        }

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 980, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Loki"
        // The thread draws its own top bar, so the system one only needs to supply the controls.
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 620, height: 420)
        window.contentView = NSHostingView(rootView: ThreadWindow(conversation: conversation))
        window.delegate = self
        window.center()
        window.setFrameAutosaveName("dev.sabharish.loki.thread")

        self.window = window
        window.makeKeyAndOrderFront(nil)
    }

    /// Closing hides the window. The conversation outlives it, so reopening resumes the thread.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }
}
