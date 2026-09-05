import SwiftUI

/// Design tokens.
///
/// **Eleven colours per theme and no others.** Every value below comes from the supplied palette.
/// Nothing here is mixed, tinted or interpolated into a twelfth colour, because the moment a
/// surface is invented the set stops being a set.
///
/// Yellow is the one accent and it means Loki: the mark, the thing being answered, the control
/// that acts. It is never used to decorate and never used for two meanings at once.
enum Theme {
    enum Colors {
        /// The window ground.
        static let background = adaptive(light: 0xF6F5F2, dark: 0x070707)
        /// Raised against the ground: sidebar, title bar, the composer.
        static let surface = adaptive(light: 0xFCFCFA, dark: 0x101010)
        /// Raised again: the user's own turn, a hovered row, a field.
        static let surfaceAlt = adaptive(light: 0xFFFFFF, dark: 0x171717)

        static let primary = adaptive(light: 0x181817, dark: 0xF1F1EF)
        static let secondary = adaptive(light: 0x62615D, dark: 0xA4A4A0)
        static let tertiary = adaptive(light: 0x8D8C87, dark: 0x70706C)

        /// The mark, and anything that acts.
        static let yellow = adaptive(light: 0xFFF12F, dark: 0xFFF12F)
        static let yellowHover = adaptive(light: 0xF2D900, dark: 0xE9DC00)
        /// Yellow at reading weight, for a filled row that must not shout.
        static let yellowSoft = adaptive(light: 0xFFF8B8, dark: 0x2A2918)
        /// What sits on yellow. Always the dark ink, in both themes, because yellow is light in both.
        static let onYellow = adaptive(light: 0x181817, dark: 0x181817)

        static let border = adaptive(light: 0xE4E2DC, dark: 0x292929)
        static let borderStrong = adaptive(light: 0xD5D2CA, dark: 0x3A3A38)
    }

    /// What the machine is doing, as a word, a glyph and an expression on the mark.
    ///
    /// **Colour is not the carrier.** The mark's face changes, and the word is always present, so
    /// the state survives greyscale, a colourblind reader and a screenshot. `released` is gone: a
    /// finished step is not a state worth a colour, and what replaced it is the thinking trace,
    /// which says how long it took instead of that it ended.
    enum State: String, CaseIterable {
        case idle
        case thinking
        case reading
        case needsYou

        var label: String {
            switch self {
            case .idle: "idle"
            case .thinking: "thinking"
            case .reading: "reading"
            case .needsYou: "needs you"
            }
        }

        var glyph: String {
            switch self {
            case .idle: "circle"
            case .thinking: "circle.dotted"
            case .reading: "circle.lefthalf.filled"
            case .needsYou: "exclamationmark"
            }
        }

        /// The same state as a background wash, for a filled row or an open trace.
        var tint: Color {
            switch self {
            case .idle: Colors.surface
            case .thinking, .reading: Colors.yellowSoft
            case .needsYou: Colors.surfaceAlt
            }
        }

        /// Yellow only where Loki is the subject. Everything else reads in the neutral ramp, or
        /// the accent would stop meaning anything.
        var color: Color {
            switch self {
            case .idle: Colors.tertiary
            case .thinking, .reading: Colors.yellow
            case .needsYou: Colors.primary
            }
        }
    }

    /// Helvetica Neue, with Inter behind it. Sans only, and nothing is monospaced: a timestamp set
    /// in mono reads as machine output, and everything here is meant to read as writing.
    enum Text {
        static let display = face(24, .semibold)
        static let title = face(17, .semibold)
        /// The record. Assistant prose, memory rows.
        static let record = face(14.5, .regular)
        /// The instrument. Controls, sidebar, labels.
        static let body = face(13.5, .regular)
        static let bodyStrong = face(13.5, .semibold)
        /// Timestamps, counts, the small print beside a control.
        static let meta = face(11.5, .medium)
        static let micro = face(10.5, .semibold)

        /// Code keeps a monospaced face, because alignment is the whole point of it.
        static let code = Font.system(size: 13, design: .monospaced)

        static func heading(_ level: Int) -> Font {
            switch level {
            case 1: face(21, .semibold)
            case 2: face(17, .semibold)
            case 3: face(15, .semibold)
            case 4: face(14, .semibold)
            case 5: face(13.5, .semibold)
            default: face(13.5, .medium)
            }
        }

        static let displayTracking = -0.022 * 24
        static let titleTracking = -0.012 * 17
        static let metaTracking = 0.01 * 11.5
        static let microTracking = 0.03 * 10.5

        static let recordLineSpacing = 14.5 * 0.62
        static let bodyLineSpacing = 13.5 * 0.5

        /// Helvetica Neue where it exists, which on macOS is everywhere, and Inter for anyone who
        /// has installed it and prefers it. The system face is the last resort rather than the
        /// first, because it is what makes an app look like every other app.
        private static func face(_ size: CGFloat, _ weight: Font.Weight) -> Font {
            for name in ["Helvetica Neue", "Inter"] where NSFont(name: name, size: size) != nil {
                return .custom(name, size: size).weight(weight)
            }
            return .system(size: size, weight: weight)
        }
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

    /// Four steps and no pills. A pill reads as a tag, and nothing here is a tag.
    enum Radius {
        static let control: CGFloat = 7
        static let panel: CGFloat = 10
        static let bubble: CGFloat = 12
        static let window: CGFloat = 14
    }

    enum Size {
        static let sidebar: CGFloat = 220
        static let inspector: CGFloat = 272
        /// Below this the inspector drops out and the timeline becomes a screen.
        static let narrow: CGFloat = 900
        /// The reading measure. Roughly 68 characters at the record size.
        static let measure: CGFloat = 640
        static let avatar: CGFloat = 24
        /// The hairline a scope or a quote is drawn against.
        static let rail: CGFloat = 2
    }

    /// One motion vocabulary, so two things that move together were told to move the same way.
    ///
    /// Everything sits between 0.18s and 0.45s. Below that a transition reads as a jump, above it
    /// the interface feels like it is thinking about the request rather than answering it.
    enum Motion {
        /// A panel opening, a column appearing. Settles without overshoot.
        static let panel = Animation.spring(response: 0.34, dampingFraction: 1.0)
        /// Anything the pointer is on: hover, press, a control changing shape.
        static let control = Animation.spring(response: 0.22, dampingFraction: 0.86)
        /// A row arriving in a list, a message landing in the thread.
        static let arrive = Animation.spring(response: 0.4, dampingFraction: 0.82)
        /// A disclosure opening or closing.
        static let disclose = Animation.spring(response: 0.3, dampingFraction: 0.9)
        /// The slow ambient loops: the blink, the drift behind the mark.
        static let ambient = Animation.easeInOut(duration: 2.4)
    }
}

/// Whether the reader has asked for less movement.
///
/// Read once and passed down rather than queried at every call site. Every looping animation in
/// the app checks it and holds a still frame instead of stopping mid-cycle, because a loop frozen
/// at a random phase looks broken rather than calm.
extension EnvironmentValues {
    @Entry var reduceMotion: Bool = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
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
