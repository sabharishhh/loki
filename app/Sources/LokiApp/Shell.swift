import LokiCore
import SwiftUI

/// The window: sidebar, thread, rail.
///
/// Three columns rather than a thread with panels bolted on, because the sidebar and the rail
/// answer different questions and neither should push the reading column around when it opens.
/// The thread keeps the centre and the columns take space from the window, not from the measure.
struct Shell: View {
    let conversation: Conversation

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
                Divider().overlay(Theme.Colors.line)

                HStack(spacing: 0) {
                    if sidebar {
                        Sidebar(conversation: conversation, screen: $screen)
                            .frame(width: Theme.Size.sidebar)
                            .transition(.move(edge: .leading).combined(with: .opacity))
                        Divider().overlay(Theme.Colors.line)
                    }

                    centre
                        .frame(maxWidth: .infinity)

                    if rail && roomy && screen == .thread {
                        Divider().overlay(Theme.Colors.line)
                        RightRail(conversation: conversation)
                            .frame(width: Theme.Size.inspector)
                            .transition(.move(edge: .trailing).combined(with: .opacity))
                    }
                }
            }
            .background(Theme.Colors.canvas)
            // One animation for the whole layout, so a column opening and the thread narrowing
            // are the same movement rather than two that nearly agree.
            .animation(Theme.Motion.panel, value: sidebar)
            .animation(Theme.Motion.panel, value: rail)
            .animation(Theme.Motion.panel, value: roomy)
            .onChange(of: roomy) { _, isRoomy in
                if !isRoomy { rail = false }
            }
        }
        .frame(minWidth: 620, minHeight: 420)
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

            Text("Loki")
                .font(Theme.Text.title)
                .kerning(Theme.Text.titleTracking)
                .foregroundStyle(Theme.Colors.ink)

            if screen == .timeline {
                Text("timeline")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.faint)
                    .padding(.horizontal, Theme.Space.xs)
                    .padding(.vertical, 1)
                    .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.control))
                    .transition(.opacity.combined(with: .scale(scale: 0.9)))
            }

            Spacer()

            Text(Money.short(conversation.spentToday) + " today")
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .monospacedDigit()
                .foregroundStyle(Theme.Colors.faint)
                .help("Spend today. core \(Core.version)")
                // Without this the whole bar shifts a pixel as digits change width.
                .contentTransition(.numericText())

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
        .background(.regularMaterial)
        .animation(Theme.Motion.standard, value: screen)
    }
}

/// A bar control. Square, quiet, and it holds its tint while on.
private struct BarToggle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button {
            configuration.isOn.toggle()
        } label: {
            configuration.label
                .font(.system(size: 12))
                .foregroundStyle(configuration.isOn ? Theme.Colors.ink : Theme.Colors.faint)
                .frame(width: 24, height: 22)
                .background(
                    configuration.isOn ? Theme.Colors.sunk : .clear,
                    in: .rect(cornerRadius: Theme.Radius.control)
                )
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .animation(Theme.Motion.standard, value: configuration.isOn)
    }
}
