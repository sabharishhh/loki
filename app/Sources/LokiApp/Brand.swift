import SwiftUI

/// **Everything about the mark, in one place.**
///
/// The artwork lives in `branding/logo/` at the repository root and that is the only place it
/// lives. Nothing is duplicated into the app target: the face below is drawn from the numbers in
/// `Geometry`, which were measured off `logo.png`, and the build script installs the same PNG as
/// the app icon. Change the artwork there and both follow.
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

    /// The face, as fractions of the circle's diameter.
    ///
    /// Measured off `logo.png` rather than eyeballed, so the drawn mark and the installed icon are
    /// the same mark. Every value is a ratio, so it holds at 15pt in a trace header and at 512pt
    /// in the Dock.
    enum Geometry {
        /// A single eye. Tall and narrow, which is most of what makes the face read as this face
        /// rather than as a generic smiley.
        static let eyeWidth: CGFloat = 0.108
        static let eyeHeight: CGFloat = 0.199
        /// Clear space between the two eyes.
        static let eyeGap: CGFloat = 0.202
        /// How far above the circle's centre the eyes sit.
        static let eyeRise: CGFloat = 0.042

        /// The highlight inside each eye.
        static let pupilDiameter: CGFloat = 0.063
        /// Inset from the top of the eye, as a fraction of the eye's height.
        static let pupilInset: CGFloat = 0.076
        /// Offset toward the outer edge, as a fraction of the eye's width. The mark is drawn
        /// looking slightly up and to the side, and centring the highlight loses that.
        static let pupilShift: CGFloat = 0.15
    }
}
