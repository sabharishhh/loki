import SwiftUI

/// **Every colour in the app. Change one here and the whole interface follows.**
///
/// Nothing anywhere else names a hex. If a surface needs a new shade, it gets added to this table
/// rather than mixed at the call site, because the moment a colour is invented in a view the set
/// stops being a set and the interface starts drifting.
///
/// The two ramps are deliberately close together. Separation comes from borders, hover and glow,
/// not from stacking greys: a panel that is three shades lighter than its ground reads as a box
/// sitting on top of the app rather than as part of it.
///
/// The dark ground is black. Everything above it was raised to match, because contrast is a
/// difference and moving one end of it without the other quietly flattens the whole table.
enum Palette {
    // MARK: - Dark. The default, and the one that is tuned.

    enum Dark {
        /// Everything sits on this. The window, the thread, the composer, the sidebar.
        ///
        /// Black, not near-black. Sabharish's call: as dark as the display will go. The cost is
        /// that the ramp above it has to work harder, which is what the rest of this table does.
        static let background: UInt32 = 0x000000
        /// Barely raised. Only for something that has to read as a distinct object.
        ///
        /// Nothing fills a shape with `background` any more. It was a legitimate shade while the
        /// ground was 0x070707, because a chip filled with it against a divider still read as one.
        /// Against black it is not a fill at all, and eight shapes across the app were drawing
        /// nothing: the toggle in the title bar, the code block, the search field, the key caps.
        static let surface: UInt32 = 0x0B0B0B
        /// The user's own turn, a field with focus.
        static let surfaceAlt: UInt32 = 0x151515

        static let primary: UInt32 = 0xF2F2F0
        static let secondary: UInt32 = 0xADADA9
        /// Times, hints, step output. Lifted off the old value because a grey tuned against
        /// 0x070707 goes muddy against black, and this is the ramp the small text lives in.
        static let tertiary: UInt32 = 0x7C7C78

        static let yellow: UInt32 = 0xFFF12F
        static let yellowHover: UInt32 = 0xE9DC00
        static let yellowSoft: UInt32 = 0x2A2918
        static let onYellow: UInt32 = 0x181817

        /// The workhorse. Almost every division in the app is one of these two hairlines.
        ///
        /// Raised with the ground. A hairline is only ever a few steps above what it sits on, and
        /// dropping the ground to black without moving these left every division fainter than it
        /// was drawn to be.
        static let border: UInt32 = 0x262626
        static let borderStrong: UInt32 = 0x454542
    }

    // MARK: - Light.

    enum Light {
        static let background: UInt32 = 0xF6F5F2
        static let surface: UInt32 = 0xFCFCFA
        static let surfaceAlt: UInt32 = 0xFFFFFF

        static let primary: UInt32 = 0x181817
        static let secondary: UInt32 = 0x62615D
        static let tertiary: UInt32 = 0x8D8C87

        static let yellow: UInt32 = 0xFFF12F
        static let yellowHover: UInt32 = 0xF2D900
        static let yellowSoft: UInt32 = 0xFFF8B8
        static let onYellow: UInt32 = 0x181817

        static let border: UInt32 = 0xE4E2DC
        static let borderStrong: UInt32 = 0xD5D2CA
    }

    /// How strongly the mark glows against the ground. Zero turns it off everywhere at once.
    static let markGlow: CGFloat = 0.32
}

/// Resolves a colour against whichever appearance the view is drawn in.
func adaptive(light: UInt32, dark: UInt32) -> Color {
    Color(nsColor: NSColor(name: nil) { appearance in
        let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        return NSColor(hex: isDark ? dark : light)
    })
}

extension NSColor {
    convenience init(hex: UInt32) {
        self.init(
            srgbRed: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            alpha: 1
        )
    }
}
