import SwiftUI

/// One turn in the thread, with everything you can do to it.
///
/// **The controls appear on hover and hold their space when they do not.** A row that grows a
/// toolbar when the pointer arrives shoves every row below it down, and reading a thread while it
/// twitches is miserable. The bar is always laid out and only its opacity changes.
///
/// Loki's turns carry the mark. The user's do not, because you know who you are.
struct MessageRow: View {
    let turn: Turn
    /// Still streaming. Keeps the mark working and holds the controls back until there is
    /// something finished to copy.
    var streaming = false
    var onEdit: ((String) -> Void)?

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var hovering = false
    @State private var editing = false
    @State private var draft = ""
    @State private var copied = false
    @FocusState private var editorFocused: Bool

    private var isUser: Bool { turn.speaker == .user }

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Space.m) {
            if isUser {
                Spacer(minLength: Theme.Space.xxl)
            } else {
                MarkBadge(state: streaming ? .thinking : .idle, size: 22, animated: streaming)
                    .padding(.top, 1)
            }

            VStack(alignment: isUser ? .trailing : .leading, spacing: Theme.Space.xs) {
                if editing {
                    editor
                } else {
                    content
                }
                footer
            }
        }
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .animation(Theme.Motion.disclose, value: editing)
        .transition(.opacity.combined(with: .offset(y: 6)))
    }

    @ViewBuilder
    private var content: some View {
        if isUser {
            Text(turn.text)
                .font(Theme.Text.record)
                .lineSpacing(Theme.Text.recordLineSpacing)
                .foregroundStyle(Theme.Colors.primary)
                .textSelection(.enabled)
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, Theme.Space.m)
                .padding(.vertical, Theme.Space.s + 1)
                .background(Theme.Colors.surfaceAlt, in: .rect(cornerRadius: Theme.Radius.bubble))
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.Radius.bubble)
                        .strokeBorder(Theme.Colors.border, lineWidth: 1)
                }
        } else {
            MarkdownText(text: turn.text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var editor: some View {
        VStack(alignment: .trailing, spacing: Theme.Space.s) {
            TextEditor(text: $draft)
                .font(Theme.Text.record)
                .scrollContentBackground(.hidden)
                .focused($editorFocused)
                .frame(minHeight: 62)
                .padding(Theme.Space.s)
                .background(Theme.Colors.surfaceAlt, in: .rect(cornerRadius: Theme.Radius.bubble))
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.Radius.bubble)
                        .strokeBorder(Theme.Colors.yellow, lineWidth: 1)
                }
            HStack(spacing: Theme.Space.s) {
                Button("Cancel") { editing = false }
                    .buttonStyle(QuietButton())
                Button("Send again") {
                    editing = false
                    onEdit?(draft)
                }
                .buttonStyle(AccentButton())
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .onAppear {
            draft = turn.text
            editorFocused = true
        }
    }

    /// The time, and the controls, on one line under the turn.
    private var footer: some View {
        HStack(spacing: Theme.Space.s) {
            if isUser { Spacer(minLength: 0) }

            Text(turn.at, format: .dateTime.hour().minute())
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .foregroundStyle(Theme.Colors.tertiary)

            if !editing {
                actions
            }

            if !isUser { Spacer(minLength: 0) }
        }
        .frame(height: 18)
    }

    private var actions: some View {
        HStack(spacing: Theme.Space.xs) {
            RowAction(
                icon: copied ? "checkmark" : "square.on.square",
                help: copied ? "Copied" : "Copy"
            ) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(turn.text, forType: .string)
                copied = true
                Task {
                    try? await Task.sleep(for: .seconds(1.6))
                    copied = false
                }
            }

            if isUser {
                RowAction(icon: "pencil", help: "Edit") {
                    draft = turn.text
                    editing = true
                }
                RowAction(icon: "arrow.clockwise", help: "Send again") {
                    onEdit?(turn.text)
                }
            }
        }
        // Laid out always, revealed on hover, so nothing below this row ever moves.
        .opacity(hovering && !streaming ? 1 : 0)
        .allowsHitTesting(hovering && !streaming)
        .animation(reduceMotion ? nil : Theme.Motion.control, value: hovering)
        .animation(Theme.Motion.control, value: copied)
    }
}

/// One small square control in a row's footer.
private struct RowAction: View {
    let icon: String
    let help: String
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 10.5, weight: .semibold))
                .foregroundStyle(hovering ? Theme.Colors.primary : Theme.Colors.tertiary)
                .frame(width: 18, height: 18)
                .background(
                    hovering ? Theme.Colors.surfaceAlt : .clear,
                    in: .rect(cornerRadius: 5)
                )
                .contentTransition(.symbolEffect(.replace))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .help(help)
        .accessibilityLabel(help)
    }
}

struct AccentButton: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Text.bodyStrong)
            .foregroundStyle(Theme.Colors.onYellow)
            .padding(.horizontal, Theme.Space.m)
            .padding(.vertical, 6)
            .background(
                hovering ? Theme.Colors.yellowHover : Theme.Colors.yellow,
                in: .rect(cornerRadius: Theme.Radius.control)
            )
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .onHover { hovering = $0 }
            .animation(Theme.Motion.control, value: hovering)
            .animation(Theme.Motion.control, value: configuration.isPressed)
    }
}

struct QuietButton: ButtonStyle {
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Text.body)
            .foregroundStyle(Theme.Colors.secondary)
            .padding(.horizontal, Theme.Space.m)
            .padding(.vertical, 6)
            .background(
                hovering ? Theme.Colors.surfaceAlt : .clear,
                in: .rect(cornerRadius: Theme.Radius.control)
            )
            .overlay {
                RoundedRectangle(cornerRadius: Theme.Radius.control)
                    .strokeBorder(Theme.Colors.border, lineWidth: 1)
            }
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .onHover { hovering = $0 }
            .animation(Theme.Motion.control, value: hovering)
    }
}
