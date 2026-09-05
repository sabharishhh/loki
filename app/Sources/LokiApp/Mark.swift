import SwiftUI

/// Loki's face, drawn to the measurements in `Brand.Geometry`.
///
/// **Drawn rather than loaded so it can move.** The mark is the status indicator: every assistant
/// in this space signals thinking with a spinner, a shimmer or three bouncing dots, all of which
/// say only that something is happening. A face says what is happening, and the same drawing works
/// at 15pt in a trace header, at 22pt beside a response and at 512pt in the Dock. A raster cannot
/// blink, and a raster scaled to 15pt turns to mush.
///
/// It is the same geometry as `branding/logo/logo.png`, which is what the app icon is built from,
/// so the thing in the Dock and the thing in the thread are one mark.
struct Mark: View {
    var state: Theme.State = .idle
    /// Turns the blink off, for a still context or a screenshot.
    var animated = true
    /// Draws the halo the artwork has. Off at small sizes, where it only muddies the edge.
    var glow = true

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var lidClosed = false
    @State private var gaze: CGFloat = 0

    /// How open the eyes are, from shut to wide.
    private var openness: CGFloat {
        if lidClosed { return 0.08 }
        switch state {
        case .idle: return 1
        case .thinking: return 0.66
        case .reading: return 0.82
        case .needsYou: return 1.14
        }
    }

    var body: some View {
        Canvas { context, size in
            let d = min(size.width, size.height)
            let origin = CGPoint(x: (size.width - d) / 2, y: (size.height - d) / 2)
            draw(in: &context, at: origin, diameter: d)
        }
        .aspectRatio(1, contentMode: .fit)
        .shadow(
            color: Theme.Colors.yellow.opacity(glow ? Palette.markGlow : 0),
            radius: glow ? 7 : 0
        )
        .animation(Theme.Motion.control, value: state)
        .animation(.easeInOut(duration: 0.085), value: lidClosed)
        .animation(Theme.Motion.ambient, value: gaze)
        .task(id: animated && !reduceMotion) {
            guard animated, !reduceMotion else {
                lidClosed = false
                gaze = 0
                return
            }
            await live()
        }
        .accessibilityHidden(true)
    }

    private func draw(in context: inout GraphicsContext, at origin: CGPoint, diameter d: CGFloat) {
        let face = CGRect(x: origin.x, y: origin.y, width: d, height: d)
        let centre = CGPoint(x: face.midX, y: face.midY)

        // The body carries the artwork's soft fall from the top left, which is what stops a flat
        // yellow disc reading as a sticker.
        context.fill(
            Path(ellipseIn: face),
            with: .radialGradient(
                Gradient(colors: [Theme.Colors.yellow, Theme.Colors.yellowHover]),
                center: CGPoint(x: face.minX + d * 0.34, y: face.minY + d * 0.3),
                startRadius: 0,
                endRadius: d * 0.78
            )
        )

        let eyeW = d * Brand.Geometry.eyeWidth
        let eyeH = d * Brand.Geometry.eyeHeight * openness
        let half = d * Brand.Geometry.eyeGap / 2
        let eyeY = centre.y - d * Brand.Geometry.eyeRise

        for side in [-1.0, 1.0] as [CGFloat] {
            let x = centre.x + side * (half + eyeW / 2)
            let eye = CGRect(x: x - eyeW / 2, y: eyeY - eyeH / 2, width: eyeW, height: eyeH)
            context.fill(Path(roundedRect: eye, cornerRadius: eyeW / 2), with: .color(.black))

            // The highlight goes once the lid is most of the way down. A dot floating on a closed
            // eye is the thing that makes a blink look wrong.
            guard openness > 0.34 else { continue }
            let pupil = d * Brand.Geometry.pupilDiameter
            let inset = d * Brand.Geometry.eyeHeight * Brand.Geometry.pupilInset
            let drift = gaze * eyeW * 0.1
            let pupilRect = CGRect(
                x: eye.midX - pupil / 2 + side * eyeW * Brand.Geometry.pupilShift + drift,
                y: eye.minY + inset,
                width: pupil,
                height: pupil
            )
            context.fill(Path(ellipseIn: pupilRect), with: .color(Theme.Colors.yellow))
        }
    }

    /// The idle loop: a blink at a human interval, and a slow drift of the gaze.
    ///
    /// Irregular on purpose. A blink on a fixed timer reads as a pulsing indicator, which is the
    /// thing this exists instead of.
    private func live() async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(.random(in: 2.8...6.5)))
            if Task.isCancelled { return }
            gaze = .random(in: -1...1)
            lidClosed = true
            try? await Task.sleep(for: .milliseconds(92))
            lidClosed = false
            if Bool.random() {
                try? await Task.sleep(for: .milliseconds(150))
                lidClosed = true
                try? await Task.sleep(for: .milliseconds(86))
                lidClosed = false
            }
        }
    }
}

/// The mark at a fixed size.
struct MarkBadge: View {
    var state: Theme.State = .idle
    var size: CGFloat = Theme.Size.avatar
    var animated = true

    var body: some View {
        Mark(state: state, animated: animated, glow: size >= 20)
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
