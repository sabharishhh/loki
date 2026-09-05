import SwiftUI

/// The composer.
///
/// The only chrome that changes shape. Four states, matching the four machine states, so the
/// border alone says what the system is doing.
///
/// Constrained to the reading measure rather than the window width. A field running the full
/// width of a wide window is hard to scan and does not match where the thread sits.
struct Composer: View {
    let conversation: Conversation

    @State private var draft = ""
    @State private var talkMonitor: Any?
    @State private var holdTimer: Task<Void, Never>?
    /// Whether F has been down for less than the hold threshold, so releasing it is a tap.
    @State private var pendingTap = false
    /// The in-flight stop. A new utterance waits for it, or it would read a stale draft.
    @State private var landing: Task<Void, Never>?
    @State private var draftBeforeTalk = ""
    @FocusState private var focused: Bool

    /// F is a letter, so a tap must type it and only a hold may start dictation.
    private static let holdThreshold = Duration.milliseconds(350)
    /// Lines before the field stops growing and starts scrolling. Dictation fills a line fast,
    /// and a one-line box makes a spoken paragraph impossible to review.
    private static let visibleLines = 8

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            field
            hints
        }
        // Capped at the reading measure and centred in the window. Sabharish's call, overriding
        // the design system's never-centre rule for the composer.
        .frame(maxWidth: Theme.Size.measure)
        .padding(.horizontal, Theme.Space.xl)
        .padding(.vertical, Theme.Space.l)
        .frame(maxWidth: .infinity)
        .background(Theme.Colors.background)
        .onAppear {
            focused = true
            watchForTalkKey()
        }
        .onDisappear(perform: stopWatching)
    }

    private var field: some View {
        HStack(alignment: .bottom, spacing: Theme.Space.m) {
            MicControl(
                recording: isRecording,
                levels: conversation.dictation.recentLevels(MicControl.bars),
                action: toggleTalking
            )

            // While dictating the field shows what has been heard, greyed, rather than being
            // replaced by a meter. Hiding the text is what made the button feel like it had
            // stopped working.
            if isRecording {
                SpokenText(committed: draftBeforeTalk, heard: conversation.dictation.transcript)
            } else {
                TextField(placeholder, text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.primary)
                    .lineLimit(1...Self.visibleLines)
                    .focused($focused)
                    .onSubmit(submit)
            }

            Button(action: primaryAction) {
                Image(systemName: isRunning ? "stop.fill" : "arrow.up")
                    .font(.system(size: 11, weight: .semibold))
                    .frame(width: 22, height: 22)
            }
            .buttonStyle(.borderless)
            .background(
                canSend ? Theme.Colors.yellow : .clear,
                in: .rect(cornerRadius: Theme.Radius.control)
            )
            .foregroundStyle(canSend ? Theme.Colors.onYellow : Theme.Colors.tertiary)
            .animation(Theme.Motion.control, value: canSend)
            .disabled(!isRunning && draft.isEmpty && !isRecording)
        }
        .padding(Theme.Space.m)
        // One step of lift when it has focus, and the ground when it does not. On a near-black
        // interface a border alone leaves the field reading as an outline drawn on nothing.
        .background(
            focused ? Theme.Colors.surfaceAlt : Theme.Colors.surface,
            in: .rect(cornerRadius: Theme.Radius.panel)
        )
        .overlay {
            RoundedRectangle(cornerRadius: Theme.Radius.panel)
                .strokeBorder(borderColor, lineWidth: borderWidth)
        }
        // The accent bleeds a little while it listens, so the state is legible from across the
        // room rather than only at the border.
        .shadow(
            color: Theme.Colors.yellow.opacity(isRecording ? 0.22 : 0),
            radius: isRecording ? 14 : 0
        )
        .animation(Theme.Motion.control, value: conversation.composer.border)
        .animation(Theme.Motion.control, value: isRecording)
        .animation(Theme.Motion.control, value: focused)
    }

    private var hints: some View {
        HStack(spacing: Theme.Space.s) {
            if isRunning {
                Key("esc")
                Text("stop").font(Theme.Text.micro).foregroundStyle(Theme.Colors.tertiary)
            }
            Key("hold F")
            Text("talk").font(Theme.Text.micro).foregroundStyle(Theme.Colors.tertiary)
            Spacer()
            Text(aside)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)
                .contentTransition(.opacity)
                .animation(Theme.Motion.control, value: aside)
        }
    }

    /// The line on the right of the hints.
    ///
    /// It said the same thing whatever was happening, which is a slogan rather than a hint. It
    /// says what the composer is doing now, and falls back to the standing note when idle.
    private var aside: String {
        if isRecording { return "listening on this Mac" }
        if isRunning { return "working" }
        return "routing by task, not by turn"
    }

    /// Whether pressing send would do anything. Drives the accent on the control.
    private var canSend: Bool {
        !isRunning && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
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

    private var isRecording: Bool { conversation.dictation.isRecording }

    private var borderColor: Color {
        if isRecording { return Theme.State.reading.color }
        return conversation.composer.border?.color ?? Theme.Colors.border
    }

    private var borderWidth: CGFloat {
        isRecording || conversation.composer.border != nil ? 1.5 : 1
    }

    private func primaryAction() {
        if isRunning { conversation.interrupt() } else { submit() }
    }

    private func submit() {
        // Sending while the mic is live would carry this recording into the next turn.
        guard !isRecording else {
            Task {
                let spoken = await conversation.stopDictation()
                sendNow((draftBeforeTalk + " " + spoken).trimmingCharacters(in: .whitespaces))
                draftBeforeTalk = ""
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

// Dictation control.
extension Composer {
    /// Hold F to talk, release to stop.
    ///
    /// The key event is never swallowed, so `f` types exactly as it always would. Only once the
    /// press passes the threshold does dictation start, and the character typed on the way in is
    /// taken back at that point.
    ///
    /// A local monitor, so this fires only while Loki is frontmost and needs no accessibility
    /// permission. `opt+space` is the global one, and it uses Carbon instead.
    private func watchForTalkKey() {
        guard talkMonitor == nil else { return }
        talkMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp]) { event in
            guard event.keyCode == 3,
                  event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty
            else { return event }
            if event.isARepeat { return nil }

            if event.type == .keyDown {
                beginHold()
            } else {
                endHold()
            }
            // F never reaches the field. Letting it through and stripping it afterwards looked
            // equivalent and was not: re-focusing after an utterance selects the whole draft, so
            // that one keystroke replaced everything already dictated. A tap types it back.
            return nil
        }
    }

    private func beginHold() {
        holdTimer?.cancel()
        pendingTap = true
        holdTimer = Task {
            try? await Task.sleep(for: Self.holdThreshold)
            guard !Task.isCancelled else { return }
            pendingTap = false
            // The previous utterance may still be landing in the draft. Reading the draft before
            // it does captures a stale prefix, and the last thing dictated is then overwritten
            // instead of appended to.
            await landing?.value
            guard !Task.isCancelled else { return }
            // Whatever is already in the draft is kept as a prefix, so holding F a second time
            // continues the sentence rather than starting one.
            startTalking(keeping: draft)
        }
    }

    private func endHold() {
        let wasTap = pendingTap
        pendingTap = false
        holdTimer?.cancel()
        holdTimer = nil

        if isRecording {
            stopTalking()
        } else if wasTap {
            // Released before the threshold, so it was a tap and F is a letter like any other.
            draft += "f"
        }
    }

    /// The mic control. Click to start, click again to stop.
    ///
    /// Hold F is faster once you know it, but a button is the only affordance a new user has.
    private func toggleTalking() {
        if isRecording {
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
        // Captured now rather than after the await, so a second hold starting in the meantime
        // cannot change which prefix this utterance appends to.
        let prefix = draftBeforeTalk
        landing = Task {
            let text = await conversation.stopDictation()
            draft = joined(prefix, text)
            draftBeforeTalk = ""
            focused = true
            placeCaretAtEnd()
        }
    }

    /// Puts the caret after the text rather than leaving the draft selected.
    ///
    /// Re-focusing a field on macOS selects its whole contents, which turns the next keystroke
    /// into a delete. Runs a tick later, because focus has to land before the field editor exists.
    private func placeCaretAtEnd() {
        DispatchQueue.main.async {
            guard let editor = NSApp.keyWindow?.firstResponder as? NSTextView else { return }
            editor.setSelectedRange(NSRange(location: editor.string.count, length: 0))
        }
    }

    /// Appends an utterance to what was already there, with one space and no stray edges.
    private func joined(_ prefix: String, _ spoken: String) -> String {
        let left = prefix.trimmingCharacters(in: .whitespaces)
        let right = spoken.trimmingCharacters(in: .whitespaces)
        if left.isEmpty { return right }
        if right.isEmpty { return left }
        return left + " " + right
    }

    private func stopWatching() {
        holdTimer?.cancel()
        holdTimer = nil
        talkMonitor.map(NSEvent.removeMonitor)
        talkMonitor = nil
    }
}

/// The mic, which becomes a level meter while recording.
///
/// One control rather than a mic plus a separate meter: the thing you pressed is the thing that
/// shows it is listening, and the composer stays free to show what was heard.
struct MicControl: View {
    /// Seven, so there is a true centre bar for the tallest reading to sit on.
    static let bars = 7

    let recording: Bool
    let levels: [Float]
    let action: () -> Void

    private static let barWidth: CGFloat = 3
    private static let maxHeight: CGFloat = 16
    private static let minHeight: CGFloat = 3

    var body: some View {
        Button(action: action) {
            Group {
                if recording {
                    meter
                } else {
                    Image(systemName: "mic")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.Colors.tertiary)
                }
            }
            .frame(width: 38, height: 22)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .animation(Theme.Motion.control, value: recording)
        .help(recording ? "Stop dictating" : "Dictate. Or hold F")
        .accessibilityLabel(recording ? "Stop dictating" : "Start dictating")
    }

    /// Bars in the accent, on nothing.
    ///
    /// **No container.** A meter in a filled box reads as a control that has changed shape, and
    /// what is wanted is the sound itself. Each bar is a full capsule rather than a rounded
    /// rectangle, so a quiet moment leaves a row of dots instead of a row of stubs.
    private var meter: some View {
        HStack(alignment: .center, spacing: 2.5) {
            ForEach(0..<Self.bars, id: \.self) { index in
                Capsule(style: .continuous)
                    .fill(Theme.Colors.yellow)
                    .frame(width: Self.barWidth, height: height(at: index))
            }
        }
        .animation(.spring(response: 0.16, dampingFraction: 0.62), value: levels)
        .transition(.opacity.combined(with: .scale(scale: 0.7)))
    }

    /// Loudest in the middle, falling away to either side.
    ///
    /// The newest reading drives the centre bar and the older ones spread outward, so the shape
    /// grows from the middle the way a voice does rather than scrolling past like a chart.
    private func height(at index: Int) -> CGFloat {
        guard !levels.isEmpty else { return Self.minHeight }
        let centre = (Self.bars - 1) / 2
        let distance = abs(index - centre)
        let reading = levels[max(levels.count - 1 - distance, 0)]
        // Shoulders sit lower than the centre even at the same reading, which is what gives the
        // group its shape instead of leaving it a flat row.
        let falloff = 1 - CGFloat(distance) * 0.22
        let level = CGFloat(reading) * falloff
        return Self.minHeight + max(level, 0) * (Self.maxHeight - Self.minHeight)
    }
}

/// What has been heard so far, filling in as you speak.
///
/// Already-typed text stays in the normal colour and the spoken part is grey, so it reads as
/// provisional until the recording stops.
struct SpokenText: View {
    let committed: String
    let heard: String

    var body: some View {
        Text(styled)
            .font(Theme.Text.body)
            .lineSpacing(Theme.Text.bodyLineSpacing)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// One string, two colours. `Text + Text` is deprecated on macOS 26.
    private var styled: AttributedString {
        var out = AttributedString()
        if !committed.isEmpty {
            var typed = AttributedString(committed + " ")
            typed.foregroundColor = Theme.Colors.primary
            out.append(typed)
        }
        var spoken = AttributedString(heard.isEmpty ? "Listening" : heard)
        spoken.foregroundColor = Theme.Colors.tertiary
        out.append(spoken)
        return out
    }
}

/// A key cap. Square-cornered, because a pill is the consumer-app signal.
struct Key: View {
    let label: String

    init(_ label: String) { self.label = label }

    var body: some View {
        Text(label)
            .font(Theme.Text.micro)
            .foregroundStyle(Theme.Colors.secondary)
            .padding(.horizontal, Theme.Space.xs)
            .padding(.vertical, 1)
            .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.control))
    }
}
