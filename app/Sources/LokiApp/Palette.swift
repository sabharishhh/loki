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
enum Palette {
    // MARK: - Dark. The default, and the one that is tuned.

    enum Dark {
        /// Everything sits on this. The window, the thread, the composer, the sidebar.
        static let background: UInt32 = 0x070707
        /// Barely raised. Only for something that has to read as a distinct object.
        static let surface: UInt32 = 0x0C0C0C
        /// The user's own turn, a hovered row, a field with focus.
        static let surfaceAlt: UInt32 = 0x141414

        static let primary: UInt32 = 0xF1F1EF
        static let secondary: UInt32 = 0xA4A4A0
        static let tertiary: UInt32 = 0x70706C

        static let yellow: UInt32 = 0xFFF12F
        static let yellowHover: UInt32 = 0xE9DC00
        static let yellowSoft: UInt32 = 0x2A2918
        static let onYellow: UInt32 = 0x181817

        /// The workhorse. Almost every division in the app is one of these two hairlines.
        static let border: UInt32 = 0x1E1E1E
        static let borderStrong: UInt32 = 0x3A3A38
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
