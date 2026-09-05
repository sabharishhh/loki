import SwiftUI

/// **Everything about the mark, in one place.**
///
/// **`branding/logo/` is the source, and the only thing to edit.** `scripts/build-app.sh` copies
/// `logo.png` to `Resources/loki-mark.png` on every build and generates the Dock icon from the
/// same file, so the mark in the thread, the mark in the title bar and the icon in the Dock are
/// all one piece of artwork. The copy is generated: change it there, not here.
///
/// It is a copy rather than a symlink because SwiftPM will not follow one into a resource bundle.
/// The build refreshing it every time is what stops it drifting.
///
/// | File | Used for |
/// |---|---|
/// | `logo.png` | The app and Dock icon, installed by `scripts/build-app.sh` |
/// | `logo-glowing.png` | The glowing variant. Not used in the app; the glow is drawn |
/// | `logo-black-bg.png` | Flattened onto black, for anywhere transparency is unwelcome |
/// | `logo-text-only.png` | The wordmark. For a banner or a README, not for the interface |
/// | `logo.ico`, `logo-glowing.ico`, `logo-text-only.ico` | Windows and favicon formats |
enum Brand {
    /// Where the artwork lives, relative to the repository root.
    static let artworkDirectory = "branding/logo"
    /// The file the app icon is generated from.
    static let iconFile = "logo.png"

    /// The mark, loaded once.
    ///
    /// **By URL, not by name.** `Image("loki-mark", bundle:)` does an asset-catalog lookup, and a
    /// loose PNG is not an asset, so it resolves to nothing and draws nothing: no crash, no
    /// warning, just a hole where the logo should be. Asking the bundle for the file is
    /// unambiguous, and the fallback below means a missing asset is loud rather than invisible.
    static let image: NSImage? = {
        let named = "loki-mark"
        let candidates: [Bundle] = [.module, .main]
        for bundle in candidates {
            if let url = bundle.url(forResource: named, withExtension: "png"),
               let image = NSImage(contentsOf: url) {
                return image
            }
        }
        // Some builds nest the resource bundle rather than flattening it.
        if let nested = Bundle.main.url(forResource: "LokiApp_LokiApp", withExtension: "bundle"),
           let bundle = Bundle(url: nested),
           let url = bundle.url(forResource: named, withExtension: "png"),
           let image = NSImage(contentsOf: url) {
            return image
        }
        return nil
    }()
}
