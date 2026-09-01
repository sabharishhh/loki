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
        // Become a Dock app before the hop. The policy change needs a run loop pass to land, and
        // activating inside the same pass finds the app still an accessory and does nothing.
        NSApp.setActivationPolicy(.regular)
        uiTrace("6 policy set regular, hopping to next runloop")
        DispatchQueue.main.async { [self] in
            uiTrace("7 present on next runloop")
            present()
        }
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

    /// Brings the window forward and puts Loki in the Dock while it is open.
    ///
    /// The app ships as `LSUIElement`, which means accessory: no Dock icon, no app menu, and
    /// never the active application. An accessory app cannot activate itself, so
    /// `NSApp.activate` alone does nothing and the window never comes forward.
    ///
    /// [`show`] switches to `.regular` a run loop pass earlier, which gives the Dock icon, the
    /// app menu, and real activation. `orderFrontRegardless` stays as the belt to that braces.
    private func reveal(_ window: NSWindow) {
        window.orderFrontRegardless()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        uiTrace(
            "8 revealed visible=\(window.isVisible) key=\(window.isKeyWindow) "
                + "active=\(NSApp.isActive) policy=\(NSApp.activationPolicy().rawValue)"
        )
    }

    /// Closing hides the window and drops Loki back to the menu bar.
    ///
    /// The conversation outlives the window, so reopening resumes the same thread.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        NSApp.setActivationPolicy(.accessory)
        uiTrace("9 window closed, back to accessory")
        return false
    }
}
