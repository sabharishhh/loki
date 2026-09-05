import SwiftUI

/// Loki's mark.
///
/// **The artwork itself, not a redrawing of it.** `Resources/loki-mark.png` is copied from
/// `branding/logo/logo.png` on every build, so the thread, the title bar, the menu bar and the
/// Dock icon are all one file. An earlier version drew the face from measured ratios, which was a
/// worse idea than it sounded: at 22pt an eye is two pixels wide and every rounding error shows.
///
/// What moves is the mark as a whole. The glow breathes while Loki is working, it leans a few
/// degrees as it starts, and it settles when it stops, so the avatar carries the state without
/// anything being drawn on top of the artwork.
struct Mark: View {
    /// The size it will be drawn at. The artwork is rendered for this size rather than resampled
    /// into it, which is what keeps it crisp.
    var size: CGFloat = Theme.Size.avatar
    var state: Theme.State = .idle
    /// Turns the ambient motion off, for a still context or a screenshot.
    var animated = true
    var glow = true

    @Environment(\.reduceMotion) private var reduceMotion
    /// Rides from 0 to 1 and back while working. Drives the glow.
    @State private var pulse: CGFloat = 0
    @State private var lean = false

    private var working: Bool { state == .thinking || state == .reading }

    var body: some View {
        artwork
            .frame(width: size, height: size)
            .pixelSnapped()
            .rotationEffect(.degrees(lean ? 5 : 0))
            // The glow alone, never a scale. `scaleEffect` rasterises the image at its layout
            // size and then stretches the bitmap, which is what made the mark look pixelated.
            .shadow(
                color: Theme.Colors.yellow.opacity(glowStrength),
                radius: 4 + pulse * 6
            )
            .animation(Theme.Motion.arrive, value: lean)
            .animation(Theme.Motion.control, value: state)
            .task(id: taskKey) { await breathe() }
            .accessibilityHidden(true)
    }

    /// The artwork, or something obviously wrong if it could not be found.
    ///
    /// A missing asset used to draw nothing at all, which is the worst outcome: the interface
    /// looks merely empty and nobody goes looking. A filled disc says the layout is right and the
    /// file is not there.
    @ViewBuilder
    private var artwork: some View {
        if let image = Brand.mark(points: size) {
            Image(nsImage: image)
        } else {
            Circle().fill(Theme.Colors.yellow)
        }
    }

    /// Brighter while working, and steady rather than dark when it is not.
    private var glowStrength: CGFloat {
        guard glow else { return 0 }
        return working ? Palette.markGlow + pulse * 0.4 : Palette.markGlow
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
        Mark(size: size, state: state, animated: animated, glow: size >= 18)
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

/// Nudges a view onto whole device pixels.
///
/// **A bitmap sampled half a pixel off is a blurred bitmap.** Integral sizes are not enough,
/// because the position matters just as much and a centred layout inside an odd-width window
/// produces a half-pixel origin every other time the window is resized. Measured at 1x: a
/// half-pixel offset costs 70 mean channel error, more than every other rendering defect here put
/// together, and it is invisible on a 2x screen because half a point is a whole pixel there.
///
/// Unnecessary once the artwork is vector (`Brand.isVector`), since Core Graphics rasterises a PDF
/// at whatever position it is given. Harmless then too, so it is not conditional.
private struct PixelSnapped: ViewModifier {
    @State private var nudge = CGSize.zero

    func body(content: Content) -> some View {
        content
            .offset(nudge)
            .background(
                GeometryReader { proxy in
                    // Reads the frame *after* the offset, so the nudge already applied has to be
                    // taken back out before computing the next one. Without that the view chases
                    // its own correction and never settles.
                    let frame = proxy.frame(in: .global)
                    Color.clear
                        .onChange(of: frame, initial: true) { _, moved in
                            settle(placedAt: moved.origin)
                        }
                }
            )
    }

    private func settle(placedAt origin: CGPoint) {
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        let unnudged = CGPoint(
            x: origin.x - nudge.width,
            y: origin.y - nudge.height
        )
        let wanted = CGSize(
            width: ((unnudged.x * scale).rounded() / scale) - unnudged.x,
            height: ((unnudged.y * scale).rounded() / scale) - unnudged.y
        )
        // Written only when it actually changes. An equal assignment is still an invalidation, and
        // this one sits behind a geometry reader that fires on every layout (B-68).
        guard abs(wanted.width - nudge.width) > 0.001
            || abs(wanted.height - nudge.height) > 0.001
        else { return }
        nudge = wanted
    }
}

extension View {
    /// See ``PixelSnapped``.
    func pixelSnapped() -> some View { modifier(PixelSnapped()) }
}
