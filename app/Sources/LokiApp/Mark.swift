import SwiftUI

/// Loki's mark.
///
/// **The artwork itself, not a redrawing of it.** `Resources/loki-mark.png` is a symlink to
/// `branding/logo/logo.png`, so the thread, the title bar and the Dock icon are all the same file
/// and none of them can drift. An earlier version drew the face from measured ratios, which was a
/// worse idea than it sounded: at 22pt the eyes are two pixels wide and every rounding error shows.
///
/// What moves is the mark as a whole. The glow breathes while Loki is working, it leans a few
/// degrees as it starts, and it settles when it stops, so the avatar carries the state without
/// anything being drawn on top of the artwork.
struct Mark: View {
    var state: Theme.State = .idle
    /// Turns the ambient motion off, for a still context or a screenshot.
    var animated = true
    var glow = true

    @Environment(\.reduceMotion) private var reduceMotion
    /// Rides from 0 to 1 and back while working. Drives the glow and the breath together.
    @State private var pulse: CGFloat = 0
    @State private var lean = false

    private var working: Bool { state == .thinking || state == .reading }

    var body: some View {
        Image("loki-mark", bundle: .module)
            .resizable()
            .interpolation(.high)
            .aspectRatio(contentMode: .fit)
            .scaleEffect(1 + pulse * 0.045)
            .rotationEffect(.degrees(lean ? 5 : 0))
            .shadow(
                color: Theme.Colors.yellow.opacity(glowStrength),
                radius: 5 + pulse * 7
            )
            .animation(Theme.Motion.arrive, value: lean)
            .animation(Theme.Motion.control, value: state)
            .task(id: taskKey) { await breathe() }
            .accessibilityHidden(true)
    }

    /// Brighter while working, and steady rather than dark when it is not.
    private var glowStrength: CGFloat {
        guard glow else { return 0 }
        let base = Palette.markGlow
        return working ? base + pulse * 0.4 : base
    }

    /// Restarts the loop when either the state or the reader's motion preference changes.
    private var taskKey: String { "\(state.rawValue)-\(animated && !reduceMotion)" }

    /// The ambient loop.
    ///
    /// A slow swell while working, and stillness otherwise. Deliberately not a spinner: the point
    /// is that you can tell at a glance whether it is busy without a control that exists only to
    /// say so, and a mark that pulses forever would be exactly that.
    private func breathe() async {
        guard animated, !reduceMotion else {
            pulse = 0
            lean = false
            return
        }
        guard working else {
            withAnimation(Theme.Motion.panel) {
                pulse = 0
                lean = false
            }
            return
        }
        // A small tilt as it starts, so the change registers even before the glow moves.
        withAnimation(Theme.Motion.arrive) { lean = true }
        try? await Task.sleep(for: .milliseconds(280))
        withAnimation(Theme.Motion.arrive) { lean = false }

        while !Task.isCancelled {
            withAnimation(.easeInOut(duration: 1.1)) { pulse = 1 }
            try? await Task.sleep(for: .milliseconds(1100))
            if Task.isCancelled { break }
            withAnimation(.easeInOut(duration: 1.1)) { pulse = 0 }
            try? await Task.sleep(for: .milliseconds(1100))
        }
        withAnimation(Theme.Motion.panel) { pulse = 0 }
    }
}

/// The mark at a fixed size.
struct MarkBadge: View {
    var state: Theme.State = .idle
    var size: CGFloat = Theme.Size.avatar
    var animated = true

    var body: some View {
        Mark(state: state, animated: animated, glow: size >= 18)
            .frame(width: size, height: size)
    }
}

#Preview("Sizes and states") {
    VStack(spacing: 28) {
        HStack(spacing: 28) {
            ForEach(Theme.State.allCases, id: \.self) { state in
                VStack(spacing: 10) {
                    MarkBadge(state: state, size: 72)
                    Text(state.label)
                        .font(Theme.Text.meta)
                        .foregroundStyle(Theme.Colors.secondary)
                }
            }
        }
        HStack(alignment: .bottom, spacing: 20) {
            ForEach([15, 22, 32, 48], id: \.self) { size in
                MarkBadge(size: CGFloat(size))
            }
        }
    }
    .padding(36)
    .background(Theme.Colors.background)
}
