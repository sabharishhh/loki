import LokiCore
import SwiftUI

/// The right rail: what memory put in play, and how the answer was built (§9.2 of the design).
///
/// Two tabs rather than two panels, because they answer the same question at different depths:
/// what did you use, and what did you do. Showing both at once makes neither readable.
struct RightRail: View {
    let conversation: Conversation

    @State private var tab: Tab = .inPlay

    enum Tab: String, CaseIterable, Identifiable {
        case inPlay = "In play"
        case trace = "Trace"

        var id: String { rawValue }
    }

    var body: some View {
        VStack(spacing: 0) {
            Tabs(selection: $tab)
            Divider().overlay(Theme.Colors.border)

            ScrollView {
                Group {
                    switch tab {
                    case .inPlay:
                        InPlay(conversation: conversation)
                    case .trace:
                        Trace(conversation: conversation)
                    }
                }
                .padding(Theme.Space.m)
            }
        }
        .background(Theme.Colors.background)
        .animation(Theme.Motion.control, value: tab)
    }
}

private struct Tabs: View {
    @Binding var selection: RightRail.Tab
    @Namespace private var underline

    var body: some View {
        HStack(spacing: 0) {
            ForEach(RightRail.Tab.allCases) { tab in
                Button {
                    selection = tab
                } label: {
                    VStack(spacing: Theme.Space.xs) {
                        Text(tab.rawValue)
                            .font(Theme.Text.body)
                            .foregroundStyle(
                                selection == tab ? Theme.Colors.primary : Theme.Colors.tertiary
                            )
                        // The underline slides between tabs rather than cutting, so the eye
                        // follows the selection instead of relocating it.
                        Group {
                            if selection == tab {
                                Rectangle()
                                    .fill(Theme.Colors.primary)
                                    .frame(height: 1.5)
                                    .matchedGeometryEffect(id: "underline", in: underline)
                            } else {
                                Color.clear.frame(height: 1.5)
                            }
                        }
                    }
                    .padding(.top, Theme.Space.s)
                    .frame(maxWidth: .infinity)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
        .animation(Theme.Motion.control, value: selection)
    }
}

/// What pre-fetch surfaced for this turn, with the control that says it is wrong.
///
/// This is where precision becomes visible: a count against the cap shows the user that memory is
/// choosing rather than dumping.
private struct InPlay: View {
    let conversation: Conversation

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            if conversation.recalled.isEmpty {
                Empty(
                    "Nothing in play",
                    detail: "When memory has something to add, it shows here before the answer."
                )
            } else {
                ForEach(conversation.recalled) { claim in
                    ClaimRow(claim: claim) {
                        conversation.markNotTrue(claim)
                    }
                }
                Text("\(conversation.recalled.count) of \(Conversation.recallCap)")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .monospacedDigit()
                    .foregroundStyle(Theme.Colors.tertiary)
                    .padding(.top, Theme.Space.xs)
            }
        }
    }
}

private struct ClaimRow: View {
    let claim: RecalledClaim
    let notTrue: () -> Void

    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            Text(claim.text)
                .font(Theme.Text.body)
                .lineSpacing(Theme.Text.bodyLineSpacing)
                .foregroundStyle(Theme.Colors.primary)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)

            HStack(spacing: Theme.Space.s) {
                Text(claim.fromSession ? "this session" : claim.name)
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.tertiary)
                    .lineLimit(1)

                Spacer(minLength: 0)

                // Only on hover: a destructive-looking control on every row makes the rail read
                // as a list of things to fix rather than a list of things it knows.
                Button(action: notTrue) {
                    Text("not true")
                        .font(Theme.Text.micro)
                        .foregroundStyle(Theme.State.needsYou.color)
                }
                .buttonStyle(.plain)
                .opacity(hovering ? 1 : 0)
                .disabled(claim.fromSession)
                .help(
                    claim.fromSession
                        ? "Said earlier in this conversation, so there is nothing stored to correct"
                        : "Drops its confidence. Nothing is deleted."
                )
            }
        }
        .padding(Theme.Space.s)
        .background(
            hovering ? Theme.Colors.surfaceAlt : Theme.Colors.surface,
            in: .rect(cornerRadius: Theme.Radius.control)
        )
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
    }
}

/// The scopes of this turn, nested, with tool output in its own well.
private struct Trace: View {
    let conversation: Conversation

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            let scopes = conversation.entries.compactMap { entry -> Scope? in
                if case let .scope(scope) = entry { return scope }
                return nil
            }
            if scopes.isEmpty {
                Empty("Nothing yet", detail: "Each step of an answer appears here as it runs.")
            } else {
                ForEach(scopes.suffix(12)) { scope in
                    ScopeRail(scope: scope)
                }
            }
        }
    }
}
