import SwiftUI

/// Loki's face, drawn rather than shipped as an image.
///
/// **Why it is a shape and not the PNG in `branding/`.** The mark is the status indicator. Every
/// other assistant signals thinking with a spinner, a shimmer or three bouncing dots, all of which
/// say only that something is happening. A face can say what is happening, and it can do it in the
/// menu bar, beside a response and in the Dock without three separate designs. A raster cannot
/// blink.
///
/// The geometry follows `branding/logo/logo.png`: a filled circle, two capsule eyes set below
/// centre, each carrying a highlight in the top half.
struct Mark: View {
    /// What the face is doing. Drives the eyes, and nothing else.
    var state: Theme.State = .idle
    /// Turns the ambient blink off, for a static context like the Dock or a screenshot.
    var animated = true

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var lidClosed = false
    @State private var gaze: CGFloat = 0

    /// How open the eyes are, from 0 shut to 1 wide.
    private var openness: CGFloat {
        if lidClosed { return 0.06 }
        switch state {
        case .idle: return 1
        // Narrowed, the way anyone concentrating narrows their eyes.
        case .thinking: return 0.62
        case .reading: return 0.78
        case .needsYou: return 1.18
        }
    }

    var body: some View {
        GeometryReader { geo in
            let side = min(geo.size.width, geo.size.height)
            ZStack {
                Circle().fill(Theme.Colors.yellow)
                HStack(spacing: side * 0.17) {
                    Eye(openness: openness, gaze: gaze)
                    Eye(openness: openness, gaze: gaze)
                }
                .frame(width: side * 0.47, height: side * 0.36)
                // Set below centre, as the mark has them.
                .offset(y: side * 0.03)
            }
            .frame(width: side, height: side)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .aspectRatio(1, contentMode: .fit)
        .animation(Theme.Motion.control, value: state)
        .animation(.easeInOut(duration: 0.09), value: lidClosed)
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

    /// The idle loop: a blink at a human interval, and a slow drift of the gaze.
    ///
    /// Irregular on purpose. A blink on a fixed timer reads as a pulsing indicator, which is the
    /// thing this exists instead of.
    private func live() async {
        while !Task.isCancelled {
            let wait = Double.random(in: 2.6...6.2)
            try? await Task.sleep(for: .seconds(wait))
            if Task.isCancelled { return }
            gaze = CGFloat.random(in: -1...1)
            lidClosed = true
            try? await Task.sleep(for: .milliseconds(94))
            lidClosed = false
            // Occasionally a second blink, which is what real eyes do.
            if Bool.random() {
                try? await Task.sleep(for: .milliseconds(150))
                lidClosed = true
                try? await Task.sleep(for: .milliseconds(88))
                lidClosed = false
            }
        }
    }
}

/// One eye: a dark capsule with a highlight riding in its upper half.
///
/// `openness` scales the capsule vertically about its own centre rather than clipping it, so a
/// blink squashes the shape the way a lid does instead of wiping it away.
private struct Eye: View {
    var openness: CGFloat
    var gaze: CGFloat

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            let h = geo.size.height
            Capsule(style: .continuous)
                .fill(Color.black)
                .overlay(alignment: .top) {
                    Circle()
                        .fill(Theme.Colors.yellow)
                        .frame(width: w * 0.74, height: w * 0.74)
                        .padding(.top, h * 0.07)
                        // The highlight drifts with the gaze, which is most of what sells it.
                        .offset(x: gaze * w * 0.11)
                        .opacity(openness < 0.3 ? 0 : 1)
                }
                .frame(width: w, height: max(h * openness, w * 0.22))
                .frame(height: h)
        }
    }
}

/// The mark at a fixed size, for a row or a toolbar.
struct MarkBadge: View {
    var state: Theme.State = .idle
    var size: CGFloat = Theme.Size.avatar
    var animated = true

    var body: some View {
        Mark(state: state, animated: animated)
            .frame(width: size, height: size)
    }
}

#Preview("States") {
    HStack(spacing: 24) {
        ForEach(Theme.State.allCases, id: \.self) { state in
            VStack(spacing: 8) {
                MarkBadge(state: state, size: 64)
                Text(state.label).font(Theme.Text.meta).foregroundStyle(Theme.Colors.secondary)
            }
        }
    }
    .padding(32)
    .background(Theme.Colors.background)
}
