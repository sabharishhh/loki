import LokiCore
import SwiftUI

/// Menu bar app. `LSUIElement` in the bundle Info.plist keeps it out of the Dock.
///
/// Phase 1 scaffold. No thread view, no scope rail, no composer yet. This exists to prove the
/// chain: Rust compiles to a static library, Swift links it, and the app calls across.
@main
struct LokiApp: App {
    var body: some Scene {
        MenuBarExtra("Loki", systemImage: "circle") {
            MenuContent()
        }
        .menuBarExtraStyle(.window)
    }
}

struct MenuContent: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Loki")
                .font(.system(size: 17, weight: .semibold))
            Text("core \(Core.version)")
                .font(.system(size: 11.5, design: .monospaced))
                .foregroundStyle(.secondary)
            Divider()
            Button("Quit") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        }
        .padding(16)
        .frame(width: 220, alignment: .leading)
    }
}
