import LokiCore
import SwiftUI

/// The window: sidebar, thread, rail.
///
/// Three columns rather than a thread with panels bolted on, because the sidebar and the rail
/// answer different questions and neither should push the reading column around when it opens.
/// The thread keeps the centre and the columns take space from the window, not from the measure.
struct Shell: View {
    let conversation: Conversation

    @State private var motion = MotionPreference()
    @State private var sidebar = false
    @State private var rail = false
    @State private var screen: Screen = .thread

    /// Below this the rail drops out and the timeline becomes a screen rather than a column.
    private static let narrow: CGFloat = Theme.Size.narrow

    enum Screen: Hashable {
        case thread
        case timeline
    }

    var body: some View {
        GeometryReader { geometry in
            let roomy = geometry.size.width >= Self.narrow

            VStack(spacing: 0) {
                TopBar(
                    conversation: conversation,
                    sidebar: $sidebar,
                    rail: $rail,
                    screen: $screen,
                    roomy: roomy
                )
                Divider().overlay(Theme.Colors.border)

                HStack(spacing: 0) {
                    if sidebar {
                        Sidebar(conversation: conversation, screen: $screen)
                            .frame(width: Theme.Size.sidebar)
                            .transition(.move(edge: .leading).combined(with: .opacity))
                        Divider().overlay(Theme.Colors.border)
                    }

                    centre
                        .frame(maxWidth: .infinity)
                        // Keyed on the screen so switching is a crossfade with a little travel,
                        // rather than one layout being replaced by another between frames.
                        .id(screen)
                        .transition(
                            .asymmetric(
                                insertion: .opacity.combined(with: .offset(y: 8)),
                                removal: .opacity
                            )
                        )

                    if rail && roomy && screen == .thread {
                        Divider().overlay(Theme.Colors.border)
                        RightRail(conversation: conversation)
                            .frame(width: Theme.Size.inspector)
                            .transition(.move(edge: .trailing).combined(with: .opacity))
                    }
                }
            }
            .background(Theme.Colors.background)
            // One animation for the whole layout, so a column opening and the thread narrowing
            // are the same movement rather than two that nearly agree.
            .animation(Theme.Motion.panel, value: sidebar)
            .animation(Theme.Motion.panel, value: rail)
            .animation(Theme.Motion.panel, value: roomy)
            .animation(Theme.Motion.disclose, value: screen)
            .onChange(of: roomy) { _, isRoomy in
                if !isRoomy { rail = false }
            }
        }
        .frame(minWidth: 620, minHeight: 420)
        .environment(\.reduceMotion, motion.reduced)
        .task { conversation.observe() }
    }

    @ViewBuilder
    private var centre: some View {
        switch screen {
        case .thread:
            VStack(spacing: 0) {
                ThreadView(conversation: conversation)
                Composer(conversation: conversation)
            }
            .transition(.opacity)
        case .timeline:
            TimelineScreen(conversation: conversation)
                .transition(.opacity)
        }
    }
}

private struct TopBar: View {
    let conversation: Conversation
    @Binding var sidebar: Bool
    @Binding var rail: Bool
    @Binding var screen: Shell.Screen
    let roomy: Bool

    var body: some View {
        HStack(spacing: Theme.Space.m) {
            // The traffic lights sit here, so the first control starts clear of them.
            Toggle(isOn: $sidebar) {
                Image(systemName: "sidebar.leading")
            }
            .toggleStyle(BarToggle())
            .help("Sessions")

            MarkBadge(size: 19, animated: false)

            Text("Loki")
                .font(Theme.Text.title)
                .kerning(Theme.Text.titleTracking)
                .foregroundStyle(Theme.Colors.primary)

            if screen == .timeline {
                Text("timeline")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.tertiary)
                    .padding(.horizontal, Theme.Space.xs)
                    .padding(.vertical, 1)
                    .background(Theme.Colors.surfaceAlt, in: .rect(cornerRadius: Theme.Radius.control))
                    .transition(.opacity.combined(with: .scale(scale: 0.9)))
            }

            Spacer()

            Meters(conversation: conversation)

            if roomy {
                Toggle(isOn: $rail) {
                    Image(systemName: "sidebar.trailing")
                }
                .toggleStyle(BarToggle())
                .help("In play")
                .disabled(screen != .thread)
                .opacity(screen == .thread ? 1 : 0.35)
            }
        }
        .padding(.leading, 78)
        .padding(.trailing, Theme.Space.l)
        .padding(.vertical, Theme.Space.m)
        // The ground, not a slab. A bar three shades lighter than the app reads as a toolbar
        // bolted on top; a hairline says the same thing and takes no light.
        .background(Theme.Colors.background)
        .animation(Theme.Motion.control, value: screen)
    }
}

/// A bar control. Square, quiet, and it holds its tint while on.
/// Cost and token spend, as one horizontal row of small readings.
///
/// A row rather than a single number because they answer different questions and a person scans
/// them together: what today cost, what this session sent and received, and how big the prompt has
/// grown. That last one is §21.3's measure. If it climbs across sessions, consolidation is letting
/// noise in, and every other symptom of that shows up months later as Loki getting vaguer.
///
/// Deliberately quiet. §5's rule is that colour means machine state, so these carry none: they are
/// a reading, not a warning, and the moment they compete with the conversation they are wrong.
private struct Meters: View {
    let conversation: Conversation

    var body: some View {
        HStack(spacing: Theme.Space.m) {
            reading(Money.short(conversation.spentToday), "today")
            Divider().frame(height: 10).overlay(Theme.Colors.border)
            reading(count(conversation.tokens.input), "in")
            reading(count(conversation.tokens.output), "out")
            reading(count(conversation.tokens.context), "ctx")
        }
        .help(
            """
            Spend today, and this session: \(conversation.tokens.calls) calls,             \(conversation.tokens.input) tokens in, \(conversation.tokens.output) out,             \(conversation.tokens.context) in the current prompt.
            core \(Core.version)
            """
        )
    }

    private func reading(_ value: String, _ label: String) -> some View {
        HStack(spacing: 3) {
            Text(value)
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.secondary)
                // Without this the whole bar shifts a pixel as digits change width.
                .contentTransition(.numericText())
            Text(label)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)
        }
    }

    /// Thousands as `12.4k`. A six-digit token count in a title bar is noise, not information.
    private func count(_ n: UInt64) -> String {
        if n < 1_000 { return String(n) }
        let thousands = Double(n) / 1_000
        return thousands < 100
            ? String(format: "%.1fk", thousands)
            : String(format: "%.0fk", thousands)
    }
}

private struct BarToggle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button {
            configuration.isOn.toggle()
        } label: {
            configuration.label
                .font(.system(size: 12))
                .foregroundStyle(configuration.isOn ? Theme.Colors.primary : Theme.Colors.tertiary)
                .frame(width: 24, height: 22)
                .background(
                    configuration.isOn ? Theme.Colors.surfaceAlt : .clear,
                    in: .rect(cornerRadius: Theme.Radius.control)
                )
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .animation(Theme.Motion.control, value: configuration.isOn)
    }
}
