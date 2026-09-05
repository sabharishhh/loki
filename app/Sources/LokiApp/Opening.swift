import SwiftUI

/// What a thread shows before anything has been said.
///
/// **Direction, not mood.** A cheerful empty state teaches people to ignore the space it occupies,
/// so there is no waving hand and no "how can I help you today". What is here is the one sentence
/// that says how this differs from a text box, and the two controls nobody discovers by looking at
/// the window.
///
/// No starter prompts either. A grid of suggested questions is the tell of an app that does not
/// trust its own field, and every one of them is a sentence the user did not want to say.
struct Opening: View {
    @Environment(\.reduceMotion) private var reduceMotion
    @State private var appeared = false

    var body: some View {
        VStack(spacing: Theme.Space.l) {
            MarkBadge(size: 56)
                .scaleEffect(appeared ? 1 : 0.92)
                .opacity(appeared ? 1 : 0)

            VStack(spacing: Theme.Space.s) {
                Text("Tell it something once")
                    .font(Theme.Text.title)
                    .kerning(Theme.Text.titleTracking)
                    .foregroundStyle(Theme.Colors.primary)

                Text("Loki keeps what you say in plain files on this Mac, and says so when it turns out to be wrong.")
                    .font(Theme.Text.record)
                    .lineSpacing(Theme.Text.recordLineSpacing)
                    .foregroundStyle(Theme.Colors.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 380)
            }
            .opacity(appeared ? 1 : 0)
            .offset(y: appeared ? 0 : 8)

            HStack(spacing: Theme.Space.l) {
                Shortcut(keys: "hold F", does: "talk instead of typing")
                Shortcut(keys: "opt space", does: "reach it from any app")
            }
            .opacity(appeared ? 1 : 0)
            .offset(y: appeared ? 0 : 10)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.Space.xxl)
        .onAppear {
            guard !reduceMotion else {
                appeared = true
                return
            }
            // Staggered by one step so the mark lands first and the words follow it, rather than
            // the whole block arriving as one slab.
            withAnimation(Theme.Motion.arrive) { appeared = true }
        }
        .accessibilityElement(children: .combine)
    }
}

/// One key and what it does. Reads as a note, not as a button.
private struct Shortcut: View {
    let keys: String
    let does: String

    var body: some View {
        HStack(spacing: Theme.Space.s) {
            Text(keys)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.secondary)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(Theme.Colors.surfaceAlt, in: .rect(cornerRadius: 5))
                .overlay {
                    RoundedRectangle(cornerRadius: 5)
                        .strokeBorder(Theme.Colors.border, lineWidth: 1)
                }
            Text(does)
                .font(Theme.Text.meta)
                .foregroundStyle(Theme.Colors.tertiary)
        }
    }
}

#Preview {
    Opening().background(Theme.Colors.background)
}
