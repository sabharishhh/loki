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
    ///
    /// Hops to the next run loop pass before doing anything. The usual caller is a button inside
    /// the menu bar popover, and that panel is still dismissing when the action fires. Ordering a
    /// window in underneath a closing panel loses the race.
    func show() {
        DispatchQueue.main.async { [self] in present() }
    }

    private func present() {
        if let window {
            reveal(window)
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
        reveal(window)
    }

    /// Brings a window forward from an app that is not, and may never become, active.
    ///
    /// `orderFrontRegardless` is the part that matters: an accessory app clicked in the menu bar
    /// is not the active application, and a plain `makeKeyAndOrderFront` from one is allowed to
    /// do nothing. Activation comes after, because there has to be a window to activate onto.
    private func reveal(_ window: NSWindow) {
        window.orderFrontRegardless()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// Closing hides the window. The conversation outlives it, so reopening resumes the thread.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }
}
