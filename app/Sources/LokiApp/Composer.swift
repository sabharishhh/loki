import SwiftUI

/// The composer.
///
/// The only chrome that changes shape. Four states, matching the four machine states, so the
/// border alone says what the system is doing.
struct Composer: View {
    let conversation: Conversation
    @State private var draft = ""
    @State private var talkMonitor: Any?
    @State private var holdTimer: Task<Void, Never>?
    @State private var draftBeforeTalk = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            HStack(spacing: Theme.Space.m) {
                Button(action: toggleTalking) {
                    Image(systemName: isListening ? "mic.fill" : "mic")
                        .font(.system(size: 13))
                        .foregroundStyle(
                            isListening ? Theme.State.reading.color : Theme.Colors.faint
                        )
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .help(isListening ? "Stop dictating" : "Dictate. Or hold F")
                .accessibilityLabel(isListening ? "Stop dictating" : "Start dictating")

                if isListening && draft.isEmpty {
                    // A waveform, not a placeholder. Silence should still look like listening.
                    Waveform(levels: conversation.dictation.levels)
                        .frame(height: 18)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    TextField(placeholder, text: $draft, axis: .vertical)
                        .textFieldStyle(.plain)
                        .font(Theme.Text.body)
                        .foregroundStyle(Theme.Colors.ink)
                        .lineLimit(1...6)
                        .focused($focused)
                        .onSubmit(submit)
                }

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
        .onAppear {
            focused = true
            watchForTalkKey()
        }
        .onDisappear(perform: stopWatching)
        .onChange(of: conversation.dictation.transcript) { _, text in
            // The field shows what was heard as it is heard, after whatever was already typed.
            if isListening {
                draft = (draftBeforeTalk + " " + text).trimmingCharacters(in: .whitespaces)
            }
        }
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
        case .needsYou: "Waiting on you"
        default: "Ask, or hold F to talk"
        }
    }

    private var isRunning: Bool {
        if case .running = conversation.composer { return true }
        return false
    }

    private var isListening: Bool { conversation.dictation.isListening }

    /// F is a letter, so a tap must type it and only a hold may start dictation.
    private static let holdThreshold = Duration.milliseconds(350)

    /// Hold F to talk, release to stop.
    ///
    /// The key event is never swallowed, so `f` types exactly as it always would. Only once the
    /// hold threshold passes does dictation start, and the character typed on the way in is taken
    /// back at that point.
    ///
    /// A local monitor, so this fires only while Loki is frontmost and needs no accessibility
    /// permission. The global hotkey does need one, which is a separate onboarding step.
    private func watchForTalkKey() {
        guard talkMonitor == nil else { return }
        talkMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp]) { event in
            guard event.keyCode == 3, event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty
            else { return event }
            if event.isARepeat { return nil }

            if event.type == .keyDown {
                beginHold()
            } else {
                endHold()
            }
            return event
        }
    }

    private func beginHold() {
        holdTimer?.cancel()
        holdTimer = Task {
            try? await Task.sleep(for: Self.holdThreshold)
            guard !Task.isCancelled else { return }
            // Take back the character the tap already typed, and keep the rest as a prefix so
            // holding F mid-sentence does not discard what is there.
            startTalking(keeping: draft.hasSuffix("f") ? String(draft.dropLast()) : draft)
        }
    }

    private func endHold() {
        holdTimer?.cancel()
        holdTimer = nil
        guard isListening else { return }
        stopTalking()
    }

    /// The mic button. Click to start, click again to stop.
    ///
    /// Hold F is faster once you know it, but a button is the only affordance a new user has.
    private func toggleTalking() {
        if isListening {
            stopTalking()
        } else {
            startTalking(keeping: draft)
        }
    }

    private func startTalking(keeping prefix: String) {
        draftBeforeTalk = prefix
        draft = prefix
        conversation.startDictation()
    }

    private func stopTalking() {
        Task {
            let text = await conversation.stopDictation()
            draft = (draftBeforeTalk + " " + text).trimmingCharacters(in: .whitespaces)
            draftBeforeTalk = ""
        }
    }

    private func stopWatching() {
        holdTimer?.cancel()
        holdTimer = nil
        talkMonitor.map(NSEvent.removeMonitor)
        talkMonitor = nil
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
        // Sending while the mic is live would carry this recording into the next turn.
        guard !isListening else {
            Task {
                let spoken = await conversation.stopDictation()
                let combined = (draftBeforeTalk + " " + spoken)
                    .trimmingCharacters(in: .whitespaces)
                draftBeforeTalk = ""
                sendNow(combined)
            }
            return
        }
        sendNow(draft)
    }

    private func sendNow(_ text: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = ""
        conversation.send(text)
    }
}

/// Live input level while dictating.
///
/// Exists because on-device transcription has a lag before the first words land, and without any
/// feedback the app looks dead while it is in fact listening.
struct Waveform: View {
    let levels: [Float]

    private static let minHeight: CGFloat = 2
    private static let maxHeight: CGFloat = 18

    var body: some View {
        // Bars share the available width rather than taking a fixed one, so the trace reaches the
        // right edge instead of stopping partway and looking truncated.
        HStack(alignment: .center, spacing: 2) {
            ForEach(0..<Dictation.waveformBars, id: \.self) { index in
                Capsule()
                    .fill(Theme.State.reading.color.opacity(opacity(at: index)))
                    .frame(maxWidth: .infinity)
                    .frame(height: height(at: index))
            }
        }
        .frame(maxWidth: .infinity)
        .animation(.linear(duration: 0.08), value: levels)
        .accessibilityLabel("Listening")
    }

    /// Newest sample at the right, so the trace runs toward the send button.
    private func level(at index: Int) -> Float {
        let offset = Dictation.waveformBars - levels.count
        guard index >= offset, index - offset < levels.count else { return 0 }
        return levels[index - offset]
    }

    private func height(at index: Int) -> CGFloat {
        Self.minHeight + CGFloat(level(at: index)) * (Self.maxHeight - Self.minHeight)
    }

    /// The oldest bars fade rather than stopping dead, so the left end reads as history running
    /// out rather than as the trace being cut.
    private func opacity(at index: Int) -> Double {
        let position = Double(index) / Double(max(Dictation.waveformBars - 1, 1))
        return 0.35 + position * 0.65
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
