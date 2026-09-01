import SwiftUI

/// Design tokens.
///
/// Colour is state, never decoration. There is no brand accent: four state colours already carry
/// meaning in the scope rail, and a fifth would make colour ambiguous. Selection uses the system
/// accent, which is achromatic under Graphite.
enum Theme {
    enum Colors {
        static let canvas = adaptive(light: 0xF1F2F4, dark: 0x101215)
        static let raised = adaptive(light: 0xFAFBFC, dark: 0x171A1E)
        static let sunk = adaptive(light: 0xE6E9ED, dark: 0x0B0D0F)
        static let ink = adaptive(light: 0x16181C, dark: 0xE9EBEF)
        static let muted = adaptive(light: 0x5A616B, dark: 0x98A0AB)
        static let faint = adaptive(light: 0x8B929C, dark: 0x666E79)
        static let line = adaptive(light: 0xDCE0E5, dark: 0x24282E)
        /// Canvas inverted, carrying the user's own words. Light box in dark mode and
        /// dark box in light mode, so the instruction reads as spoken input, not record.
        static let inverted = adaptive(light: 0x1B1E23, dark: 0xE9EBEF)
        static let onInverted = adaptive(light: 0xF1F2F4, dark: 0x16181C)
    }

    /// The four machine states. Each pairs with a glyph and a word, so colour is never the only cue.
    enum State: String, CaseIterable {
        case holding
        case reading
        case released
        case needsYou

        var label: String {
            switch self {
            case .holding: "holding"
            case .reading: "reading"
            case .released: "released"
            case .needsYou: "needs you"
            }
        }

        var glyph: String {
            switch self {
            case .holding: "square.fill"
            case .reading: "circle"
            case .released: "checkmark"
            case .needsYou: "square.lefthalf.filled"
            }
        }

        var color: Color {
            switch self {
            case .holding: adaptive(light: 0xA96A00, dark: 0xE0A33C)
            case .reading: adaptive(light: 0x1B6FA8, dark: 0x6BADDD)
            case .released: adaptive(light: 0x3F6B4F, dark: 0x7CB791)
            case .needsYou: adaptive(light: 0xB23A17, dark: 0xE88A63)
            }
        }

        var tint: Color {
            switch self {
            case .holding: adaptive(light: 0xF7EFE1, dark: 0x241C0E)
            case .reading: adaptive(light: 0xE8F0F7, dark: 0x0E1A24)
            case .released: adaptive(light: 0xEBF1ED, dark: 0x101C14)
            case .needsYou: adaptive(light: 0xF8ECE7, dark: 0x24120C)
            }
        }
    }

    /// Tracking changes with size, per Apple's typography guidance. One value everywhere is wrong
    /// somewhere.
    enum Text {
        static let display = Font.system(size: 24, weight: .semibold)
        static let title = Font.system(size: 17, weight: .semibold)
        /// The record. Assistant prose and timeline rows.
        static let record = Font.system(size: 15)
        /// The instrument. UI, steps, sidebar.
        static let body = Font.system(size: 13.5)
        static let meta = Font.system(size: 11.5, design: .monospaced)
        static let micro = Font.system(size: 10.5, weight: .medium, design: .monospaced)

        static let displayTracking = -0.021 * 24
        static let titleTracking = -0.011 * 17
        static let metaTracking = 0.012 * 11.5
        static let microTracking = 0.03 * 10.5

        static let recordLineSpacing = 15 * 0.7
        static let bodyLineSpacing = 13.5 * 0.55
    }

    /// 4px base, 8px rhythm.
    enum Space {
        static let xs: CGFloat = 4
        static let s: CGFloat = 8
        static let m: CGFloat = 12
        static let l: CGFloat = 16
        static let xl: CGFloat = 24
        static let xxl: CGFloat = 32
        static let xxxl: CGFloat = 48
    }

    /// One scale, no exceptions, no pills. A square-cornered badge reads as a readout.
    enum Radius {
        static let control: CGFloat = 6
        static let panel: CGFloat = 10
        static let window: CGFloat = 14
        /// The user's turn. The same 14px step as the window rather than a fourth value, so the
        /// radius lock stays a three-value scale and the box stops short of being a pill.
        static let bubble: CGFloat = 14
    }

    enum Size {
        static let sidebar: CGFloat = 216
        static let inspector: CGFloat = 264
        /// Below this the inspector drops out and the timeline becomes a screen.
        static let narrow: CGFloat = 900
        /// 68ch at the record size, near enough.
        static let measure: CGFloat = 660
        static let rail: CGFloat = 2
    }

    /// Springs on anything the user can grab. Nothing decorative.
    enum Motion {
        static let panel = Animation.spring(response: 0.35, dampingFraction: 1.0)
        static let sheet = Animation.spring(response: 0.3, dampingFraction: 0.8)
        static let standard = Animation.spring(response: 0.3, dampingFraction: 1.0)
    }
}

private func adaptive(light: UInt32, dark: UInt32) -> Color {
    Color(nsColor: NSColor(name: nil) { appearance in
        let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        return NSColor(hex: isDark ? dark : light)
    })
}

private extension NSColor {
    convenience init(hex: UInt32) {
        self.init(
            srgbRed: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            alpha: 1
        )
    }
}
