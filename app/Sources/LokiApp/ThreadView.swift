import SwiftUI

/// The thread.
///
/// The user's turn sits right in an inverted box and the assistant's runs full measure with no
/// container. Two sides, one spine: only the instruction is boxed, so the record still reads as
/// prose rather than as a chat log.
struct ThreadView: View {
    let conversation: Conversation

    /// Whether the app is tracking the end of the thread.
    ///
    /// **Set by sending, cleared only by scrolling up on purpose.** Gating the follow on "is the
    /// end currently on screen" was wrong in both directions: sending a message from halfway up
    /// the thread left the reader looking at an old answer with no sign anything had happened, and
    /// the space this view opens under a new question puts the end off screen the instant it is
    /// asked, which switched the follow off for the whole reply.
    @State private var following = true
    /// Whether the end of the thread is on screen. Drives the caret, nothing else.
    @State private var atBottom = true
    /// The last geometry seen, so a scroll the reader performed can be told from content growing
    /// underneath them.
    @State private var seen = Geometry()
    /// The viewport, for the room a new question needs under it.
    @State private var viewport: CGFloat = 0

    /// What a scroll needs to know about itself.
    private struct Geometry: Equatable {
        var offset: CGFloat = 0
        var content: CGFloat = 0
        var container: CGFloat = 0
    }

    var body: some View {
        if conversation.entries.isEmpty && conversation.lastError == nil {
            Opening()
                .transition(.opacity)
                .background(Theme.Colors.background)
        } else {
            thread
        }
    }

    private var thread: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.Space.xl) {
                    ForEach(conversation.entries) { entry in
                        switch entry {
                        case .turn(let turn):
                            MessageRow(
                                turn: turn,
                                streaming: conversation.isStreaming(turn),
                                onEdit: { text in
                                    conversation.resend(from: turn.id, text: text)
                                }
                            )
                            .id(turn.id)
                        case .scope(let scope):
                            ThinkingTrace(scope: scope, live: conversation.isLive(scope))
                                .id(scope.id)
                        }
                    }

                    if let error = conversation.lastError {
                        ErrorRow(message: error)
                    }

                    if !conversation.summary.isEmpty {
                        SessionSummary(lines: conversation.summary)
                    }

                    // Room for the answer to arrive into.
                    //
                    // A question sent from the bottom of a full window has nowhere to go: it can
                    // be scrolled to the bottom edge and no further, so it sits under the composer
                    // with the reply forming below the fold. This opens most of a viewport beneath
                    // it while the turn runs, which is what lets the question rise to the top and
                    // the answer be read where it lands. It closes again once the turn is done.
                    Color.clear
                        .frame(height: tailRoom)
                        .id(Self.tailID)
                }
                // Capped at the measure, padded, then centred, in that order. The same three
                // steps the composer takes, so the column's edges line up with the field's and
                // a right-set turn lands on the same rule as the send button.
                .frame(maxWidth: Theme.Size.measure)
                .padding(.horizontal, Theme.Space.xl)
                .padding(.vertical, Theme.Space.xxl)
                .frame(maxWidth: .infinity)
            }
            .onScrollGeometryChange(for: Geometry.self) { geometry in
                Geometry(
                    offset: geometry.contentOffset.y,
                    content: geometry.contentSize.height,
                    container: geometry.containerSize.height
                )
            } action: { _, now in
                react(to: now, proxy: proxy)
            }
            .onChange(of: conversation.entries.count) {
                arrived(proxy: proxy)
            }
            .overlay(alignment: .bottom) {
                if !atBottom {
                    ScrollDown {
                        following = true
                        withAnimation(Theme.Motion.panel) {
                            proxy.scrollTo(Self.tailID, anchor: .bottom)
                        }
                    }
                    .padding(.bottom, Theme.Space.l)
                    .transition(.opacity.combined(with: .offset(y: 8)))
                }
            }
            .animation(Theme.Motion.control, value: atBottom)
            .animation(Theme.Motion.panel, value: conversation.working)
        }
        .background(Theme.Colors.background)
    }

    /// The end of the stack. Only the caret jumps here; the follow pins the last real entry.
    private static let tailID = "thread-tail"

    /// How much empty space sits under the newest turn while one is running.
    ///
    /// Not a whole viewport. A question that rises to the very top with nothing under it reads as
    /// the thread having been cleared, and the answer then arrives at the top of an empty screen.
    private var tailRoom: CGFloat { conversation.working ? viewport * 0.62 : 0 }

    /// The last thing in the thread that is actually a turn or a trace.
    private var lastEntryID: AnyHashable? {
        switch conversation.entries.last {
        case .turn(let turn): AnyHashable(turn.id)
        case .scope(let scope): AnyHashable(scope.id)
        case nil: nil
        }
    }

    /// Reads one scroll.
    ///
    /// Two things arrive through the same callback and mean opposite things. Content growing under
    /// a still pointer is the answer streaming in, and the follow should hold. The offset moving up
    /// while the content is the same size is the reader going back for something, and the follow
    /// should stop. Telling them apart is the whole of the logic.
    private func react(to now: Geometry, proxy: ScrollViewProxy) {
        viewport = now.container
        // The room under the newest turn is not content, so reaching the last word counts as
        // reaching the end even with half a screen of nothing below it.
        let end = now.content - tailRoom
        let fold = now.offset + now.container
        atBottom = fold >= end - 24
        defer { seen = now }

        if now.content == seen.content, now.offset < seen.offset - 2 {
            following = false
            return
        }
        if atBottom {
            following = true
            return
        }
        // The answer has outgrown the room left for it. Keep its last line on the fold rather than
        // pinning the empty tail, which would push the question that was asked off the top.
        if following, now.content > seen.content, let last = lastEntryID {
            proxy.scrollTo(last, anchor: .bottom)
        }
    }

    /// A turn was added.
    ///
    /// A question goes to the top of the window and the answer fills in underneath it, which is
    /// the one placement that keeps both on screen. Anything else is a new turn arriving where the
    /// reader is not looking.
    private func arrived(proxy: ScrollViewProxy) {
        guard case .turn(let turn) = conversation.entries.last, turn.speaker == .user else { return }
        following = true
        // A tick later, because the row and the room under it do not exist until this update has
        // been laid out, and scrolling to a view that has not been placed yet does nothing.
        Task { @MainActor in
            withAnimation(Theme.Motion.panel) { proxy.scrollTo(turn.id, anchor: .top) }
        }
    }
}

private struct TurnView: View {
    /// How much of the column a user turn leaves clear on its left. Sets where a long one wraps.
    private static let userTurnGutter: CGFloat = 0.25

    let turn: Turn

    var body: some View {
        switch turn.speaker {
        case .user:
            // A leading spacer, not a trailing frame. `frame(maxWidth:alignment:)` expands to
            // its maximum and then aligns the text inside that box, so a short turn ended up a
            // whole cap-width short of the right edge. A spacer lets the box hug its text and
            // pushes it against the column edge, and its minimum is what makes a long turn wrap.
            HStack(spacing: 0) {
                Spacer(minLength: Theme.Size.measure * Self.userTurnGutter)
                Text(turn.text)
                    .font(Theme.Text.body)
                    .lineSpacing(Theme.Text.bodyLineSpacing)
                    .foregroundStyle(Theme.Colors.primary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, Theme.Space.l)
                    .padding(.vertical, Theme.Space.m)
                    .background(
                        Theme.Colors.surfaceAlt,
                        in: .rect(cornerRadius: Theme.Radius.bubble)
                    )
            }

        case .assistant:
            // Full measure, no container. Prose, because it is prose.
            MarkdownText(text: turn.text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct ErrorRow: View {
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Space.s) {
            Image(systemName: Theme.State.needsYou.glyph)
                .font(.system(size: 10))
                .foregroundStyle(Theme.State.needsYou.color)
            Text(message)
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.primary)
        }
        .padding(Theme.Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.State.needsYou.tint, in: .rect(cornerRadius: Theme.Radius.control))
    }
}

/// What was learned this session (§17.4).
///
/// Inline at the end of the thread, not a modal. Up to three lines, and it never appears at all
/// when nothing happened: a card that says "learned nothing today" teaches people to ignore the
/// card, which costs the differentiator its only daily showing.
struct SessionSummary: View {
    let lines: [String]

    @State private var shown = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Text("Learned today")
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)

            ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                Text(line)
                    .font(Theme.Text.body)
                    .lineSpacing(Theme.Text.bodyLineSpacing)
                    .foregroundStyle(Theme.Colors.primary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Theme.Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.Colors.surface, in: .rect(cornerRadius: Theme.Radius.panel))
        .overlay(alignment: .leading) {
            // The same rail the thread uses, so the card reads as part of the record rather than
            // as a notification arriving on top of it.
            Rectangle()
                .fill(Theme.Colors.border)
                .frame(width: Theme.Size.rail)
        }
        .clipShape(.rect(cornerRadius: Theme.Radius.panel))
        .opacity(shown ? 1 : 0)
        .offset(y: shown ? 0 : 8)
        .onAppear { withAnimation(Theme.Motion.panel) { shown = true } }
    }
}

/// The way back to the newest turn.
///
/// Appears only when the end is off screen. A control that is always there is a control nobody
/// reads, and this one has to be noticed the one time it matters.
private struct ScrollDown: View {
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: "arrow.down")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(hovering ? Theme.Colors.onYellow : Theme.Colors.secondary)
                .frame(width: 26, height: 26)
                .background(
                    hovering ? Theme.Colors.yellow : Theme.Colors.surface,
                    in: .circle
                )
                .overlay {
                    Circle().strokeBorder(
                        hovering ? .clear : Theme.Colors.border,
                        lineWidth: 1
                    )
                }
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
        .help("Jump to the newest")
        .accessibilityLabel("Jump to the newest message")
    }
}
