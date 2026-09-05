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
}
