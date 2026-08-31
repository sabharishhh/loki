import SwiftUI

/// The composer.
///
/// The only chrome that changes shape. Four states, matching the four machine states, so the
/// border alone says what the system is doing.
struct Composer: View {
    let conversation: Conversation
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            HStack(spacing: Theme.Space.m) {
                Image(systemName: "mic")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.Colors.faint)

                TextField(placeholder, text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.ink)
                    .lineLimit(1...6)
                    .focused($focused)
                    .onSubmit(submit)

                Button(action: primaryAction) {
                    Image(systemName: isRunning ? "stop.fill" : "arrow.up")
                        .font(.system(size: 11, weight: .semibold))
                        .frame(width: 22, height: 22)
                }
                .buttonStyle(.borderless)
                .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.control))
                .disabled(!isRunning && draft.isEmpty)
            }
            .padding(Theme.Space.m)
            .background(Theme.Colors.raised, in: .rect(cornerRadius: Theme.Radius.control))
            .overlay {
                RoundedRectangle(cornerRadius: Theme.Radius.control)
                    .strokeBorder(borderColor, lineWidth: borderWidth)
            }
            .animation(Theme.Motion.standard, value: conversation.composer.border)

            hints
        }
        .padding(Theme.Space.l)
        .background(.regularMaterial)
        .onAppear { focused = true }
    }

    private var hints: some View {
        HStack(spacing: Theme.Space.s) {
            if isRunning {
                Key("esc")
                Text("stop").font(Theme.Text.micro).foregroundStyle(Theme.Colors.faint)
            }
            Key("hold F")
            Text("talk").font(Theme.Text.micro).foregroundStyle(Theme.Colors.faint)
            Spacer()
            Text("routing by task, not by turn")
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.faint)
        }
    }

    private var placeholder: String {
        switch conversation.composer {
        case .listening: "Listening"
        case .needsYou: "Waiting on you"
        default: "Ask, or hold F to talk"
        }
    }

    private var isRunning: Bool {
        if case .running = conversation.composer { return true }
        return false
    }

    private var borderColor: Color {
        conversation.composer.border?.color ?? Theme.Colors.line
    }

    private var borderWidth: CGFloat {
        conversation.composer.border == nil ? 1 : 1.5
    }

    private func primaryAction() {
        if isRunning { conversation.interrupt() } else { submit() }
    }

    private func submit() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = ""
        conversation.send(text)
    }
}

/// A key cap. Square-cornered, because a pill is the consumer-app signal.
struct Key: View {
    let label: String

    init(_ label: String) { self.label = label }

    var body: some View {
        Text(label)
            .font(Theme.Text.micro)
            .foregroundStyle(Theme.Colors.muted)
            .padding(.horizontal, Theme.Space.xs)
            .padding(.vertical, 1)
            .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.control))
    }
}
