import SwiftUI

/// Sessions by day, connectors underneath (§9.1 of the design system).
///
/// Sessions are read off `episodes/`, because that is where a session actually is. A separate list
/// would be a second record to keep in step with the first.
struct Sidebar: View {
    let conversation: Conversation
    @Binding var screen: Shell.Screen

    @State private var sessions: [String] = []
    @State private var loading = true

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Space.l) {
                    Section("memory") {
                        Row(
                            label: "Timeline",
                            glyph: "clock.arrow.circlepath",
                            selected: screen == .timeline
                        ) {
                            screen = .timeline
                        }
                        Row(label: "This session", glyph: "bubble.left", selected: screen == .thread) {
                            screen = .thread
                        }
                    }

                    Section("sessions") {
                        if loading {
                            // Skeletons, not a spinner: a spinner says wait, a skeleton says what
                            // is coming and how much of it.
                            ForEach(0..<3, id: \.self) { _ in
                                Skeleton(width: .random(in: 0.55...0.9))
                            }
                        } else if sessions.isEmpty {
                            Empty(
                                "No sessions yet",
                                detail: "Everything you say is kept as a dated file you can open."
                            )
                        } else {
                            ForEach(sessions, id: \.self) { day in
                                Row(label: pretty(day), glyph: "calendar", selected: false) {
                                    screen = .timeline
                                }
                            }
                        }
                    }

                    Section("connected") {
                        Empty(
                            "Nothing connected",
                            detail: "Mail and calendar arrive with actions."
                        )
                    }
                }
                .padding(Theme.Space.m)
            }
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .background(Theme.Colors.background)
        .task {
            sessions = conversation.sessions()
            // A store that answers instantly should not flash a skeleton, so the state changes
            // together with the content rather than a frame later.
            withAnimation(Theme.Motion.control) { loading = false }
        }
    }

    /// `2026-09-01` reads as a filename. `1 September` reads as a day.
    private func pretty(_ day: String) -> String {
        let parts = day.split(separator: "-")
        guard parts.count >= 3, let month = Int(parts[1]), let date = Int(parts[2]),
              (1...12).contains(month)
        else { return day }
        let names = [
            "January", "February", "March", "April", "May", "June",
            "July", "August", "September", "October", "November", "December",
        ]
        return "\(date) \(names[month - 1])"
    }
}

private struct Section<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            Text(title)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)
                .padding(.horizontal, Theme.Space.s)
                .padding(.bottom, Theme.Space.xs)
            content
        }
    }
}

private struct Row: View {
    let label: String
    let glyph: String
    let selected: Bool
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.Space.s) {
                Image(systemName: glyph)
                    .font(.system(size: 11))
                    .foregroundStyle(selected ? Theme.Colors.primary : Theme.Colors.tertiary)
                    .frame(width: 14)
                Text(label)
                    .font(Theme.Text.body)
                    .foregroundStyle(selected ? Theme.Colors.primary : Theme.Colors.secondary)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Theme.Space.s)
            .padding(.vertical, Theme.Space.xs + 1)
            .background(
                background,
                in: .rect(cornerRadius: Theme.Radius.control)
            )
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .animation(Theme.Motion.control, value: selected)
    }

    private var background: Color {
        if selected { return Theme.Colors.background }
        return hovering ? Theme.Colors.background.opacity(0.5) : .clear
    }
}

/// An empty state that names the next action rather than reporting absence.
struct Empty: View {
    let title: String
    let detail: String

    init(_ title: String, detail: String) {
        self.title = title
        self.detail = detail
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.secondary)
            Text(detail)
                .font(Theme.Text.micro)
                .foregroundStyle(Theme.Colors.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, Theme.Space.s)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A loading placeholder, shaped like the thing that is coming.
///
/// A slow sweep rather than a pulse: a pulse reads as an alert, a sweep reads as work.
struct Skeleton: View {
    var width: CGFloat = 0.8
    var height: CGFloat = 12

    @State private var shift: CGFloat = -1

    var body: some View {
        GeometryReader { geometry in
            RoundedRectangle(cornerRadius: 3)
                .fill(Theme.Colors.background)
                .frame(width: geometry.size.width * width, height: height)
                .overlay(alignment: .leading) {
                    LinearGradient(
                        colors: [.clear, Theme.Colors.border.opacity(0.7), .clear],
                        startPoint: .leading,
                        endPoint: .trailing
                    )
                    .frame(width: geometry.size.width * width * 0.4)
                    .offset(x: geometry.size.width * width * shift)
                }
                .clipShape(.rect(cornerRadius: 3))
                .padding(.horizontal, Theme.Space.s)
        }
        .frame(height: height)
        .onAppear {
            withAnimation(.linear(duration: 1.1).repeatForever(autoreverses: false)) {
                shift = 1.6
            }
        }
        .accessibilityHidden(true)
    }
}
