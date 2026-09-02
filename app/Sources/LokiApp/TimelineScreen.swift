import AppKit
import LokiCore
import SwiftUI

/// What Loki knows (§17.3). The trust surface for the whole product.
///
/// **Rows are what Loki knows, not a stream of state transitions.** The first build rendered
/// `log.md`, which is a chronological record of things that happened to the store, and it read as
/// broken atomic pieces listed linearly. A log answers "what changed". This has to answer "what do
/// you think you know", so it is grouped by the thing each fact is about.
///
/// **No internal state name appears anywhere below.** Not `draft`, not `candidate`, not "noted,
/// not yet used". A person is owed the consequence, which is whether Loki is using something, and
/// never the vocabulary.
///
/// Correctness under the hood is not what a user feels. Being able to check the work is, which is
/// why every row can open the file it came from.
struct TimelineScreen: View {
    let conversation: Conversation

    @State private var entities: [KnownEntity] = []
    @State private var search = ""
    @State private var loading = true

    private var shown: [KnownEntity] {
        let needle = search.trimmingCharacters(in: .whitespaces).lowercased()
        guard !needle.isEmpty else { return entities }
        return entities.filter { $0.mentions(needle) }
    }

    private var openQuestions: Int {
        entities.reduce(0) { $0 + $1.questions.count }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Space.xl) {
                header

                if loading {
                    VStack(alignment: .leading, spacing: Theme.Space.m) {
                        ForEach(0..<4, id: \.self) { n in
                            Skeleton(width: [0.9, 0.7, 0.85, 0.6][n], height: 14)
                        }
                    }
                } else if entities.isEmpty {
                    Empty(
                        "Nothing yet",
                        detail: "Tell Loki something about yourself and it appears here, "
                            + "with what it replaced and how long it was wrong."
                    )
                } else if shown.isEmpty {
                    Empty("No match", detail: "Nothing here mentions \"\(search)\".")
                } else {
                    ForEach(shown) { entity in
                        EntityCard(entity: entity, conversation: conversation, reload: reload)
                    }
                }
            }
            .frame(maxWidth: Theme.Size.measure)
            .padding(.horizontal, Theme.Space.xl)
            .padding(.vertical, Theme.Space.xxl)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.Colors.raised)
        .task {
            entities = conversation.knowledge()
            withAnimation(Theme.Motion.standard) { loading = false }
        }
    }

    private func reload() {
        withAnimation(Theme.Motion.standard) { entities = conversation.knowledge() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            VStack(alignment: .leading, spacing: Theme.Space.xs) {
                Text("What Loki knows")
                    .font(Theme.Text.display)
                    .kerning(Theme.Text.displayTracking)
                    .foregroundStyle(Theme.Colors.ink)
                Text(subtitle)
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.muted)
            }

            // Search appears once there is enough here to need it. Before that it is a control
            // asking to be used on four rows, which is noise.
            if entities.count > 4 {
                SearchField(text: $search)
            }
        }
    }

    private var subtitle: String {
        if openQuestions == 1 {
            return "One thing needs you. Every row is a line in a file you can open."
        }
        if openQuestions > 1 {
            return "\(openQuestions) things need you. Every row is a line in a file you can open."
        }
        return "Grouped by what it is about. Every row is a line in a file you can open."
    }
}

/// One entity and everything known about it.
private struct EntityCard: View {
    let entity: KnownEntity
    let conversation: Conversation
    let reload: () -> Void

    @State private var appeared = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            heading

            ForEach(entity.questions) { question in
                QuestionRow(
                    entity: entity,
                    question: question,
                    conversation: conversation,
                    reload: reload
                )
            }

            ForEach(entity.facts) { fact in
                FactRow(entity: entity, fact: fact, conversation: conversation, reload: reload)
            }
        }
        .padding(Theme.Space.l)
        .background(Theme.Colors.canvas, in: .rect(cornerRadius: Theme.Radius.panel))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.panel)
                .stroke(Theme.Colors.line, lineWidth: 1)
        )
        .opacity(appeared ? 1 : 0)
        .offset(y: appeared ? 0 : 6)
        .onAppear { withAnimation(Theme.Motion.standard) { appeared = true } }
    }

    private var heading: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s) {
            Text(entity.name)
                .font(Theme.Text.title)
                .kerning(Theme.Text.titleTracking)
                .foregroundStyle(Theme.Colors.ink)

            Text(entity.kind)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.faint)

            if entity.confirmed {
                Text("you confirmed this")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.State.released.color)
            } else if !entity.inUse && entity.questions.isEmpty {
                // The consequence, not the state name: what matters is that Loki is holding this
                // back, not that a field somewhere says `draft`.
                Text("not in use yet")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.State.holding.color)
            }

            Spacer(minLength: 0)

            Button { reveal(entity.path) } label: {
                Text("open file")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.Colors.faint)
            .help("Show \(entity.path) in Finder")
        }
    }
}

/// One fact, with what it replaced folded onto the same row.
///
/// §5's rule that colour means machine state holds here, so the emotional centre of the product
/// gets no hue at all and is carried by typography.
private struct FactRow: View {
    let entity: KnownEntity
    let fact: KnownFact
    let conversation: Conversation
    let reload: () -> Void

    @State private var hovering = false
    @State private var editing = false
    @State private var draft = ""

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Space.m) {
            Rectangle()
                .fill(Theme.Colors.line)
                .frame(width: Theme.Size.rail)
                .frame(maxHeight: .infinity)

            VStack(alignment: .leading, spacing: Theme.Space.xs) {
                label

                if editing {
                    editor
                } else {
                    Text(fact.text)
                        .font(Theme.Text.record)
                        .lineSpacing(Theme.Text.recordLineSpacing)
                        .foregroundStyle(Theme.Colors.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if let since = fact.since {
                    Text(since)
                        .font(Theme.Text.meta)
                        .kerning(Theme.Text.metaTracking)
                        .foregroundStyle(Theme.Colors.faint)
                }

                if let was = fact.was {
                    correction(was)
                }

                if hovering && !editing {
                    actions
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .fixedSize(horizontal: false, vertical: true)
        .onHover { hovering = $0 }
    }

    private var label: some View {
        HStack(spacing: Theme.Space.s) {
            if !fact.attribute.isEmpty {
                Text(fact.attribute.replacingOccurrences(of: "_", with: " "))
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.faint)
            }
            if fact.fromElsewhere {
                Text("I read this somewhere")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.State.holding.color)
            }
        }
    }

    /// The superseded half. Quieter than the live one, and it says how long Loki was wrong.
    private func correction(_ was: Superseded) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(was.text)
                .font(Theme.Text.body)
                .strikethrough(true, color: Theme.Colors.faint)
                .foregroundStyle(Theme.Colors.muted)
                .fixedSize(horizontal: false, vertical: true)
            Text(wrongLine(was))
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .foregroundStyle(Theme.Colors.faint)
        }
        .padding(.top, Theme.Space.xs)
    }

    private func wrongLine(_ was: Superseded) -> String {
        guard let wrongFor = was.wrongFor else { return was.held }
        return "\(was.held). I was wrong about it for \(wrongFor)."
    }

    private var editor: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            TextField("", text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .font(Theme.Text.record)
                .foregroundStyle(Theme.Colors.ink)
                .padding(Theme.Space.s)
                .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.control))

            HStack(spacing: Theme.Space.m) {
                Quiet("save") {
                    let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    editing = false
                    guard !text.isEmpty, text != fact.text else { return }
                    conversation.amend(path: entity.path, ordinal: fact.ordinal, text: text)
                    reload()
                }
                Quiet("cancel") { editing = false }
            }
        }
    }

    private var actions: some View {
        HStack(spacing: Theme.Space.m) {
            Quiet("edit") {
                draft = fact.text
                editing = true
            }
            Quiet("this is wrong") {
                conversation.forget(path: entity.path, ordinal: fact.ordinal)
                reload()
            }
        }
        .padding(.top, Theme.Space.xs)
    }
}

/// A conflict, rendered as the question it is (§9.7 rule 4, §9.8's one tap).
///
/// The store deliberately refuses to guess when two things you said cannot both be true, so it
/// holds both and asks. Until someone answers, the whole entity stays out of use, which makes this
/// the one row that has to be actionable rather than informative.
private struct QuestionRow: View {
    let entity: KnownEntity
    let question: OpenQuestion
    let conversation: Conversation
    let reload: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Text("Which is right?")
                .font(Theme.Text.bodyStrong)
                .foregroundStyle(Theme.Colors.ink)

            Text("You told me both, and they cannot both be true. I am not using either until you say.")
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.muted)
                .fixedSize(horizontal: false, vertical: true)

            ForEach(question.options) { option in
                Button {
                    conversation.settle(path: entity.path, keep: option.ordinal)
                    reload()
                } label: {
                    HStack(alignment: .top, spacing: Theme.Space.s) {
                        Text(option.text)
                            .font(Theme.Text.record)
                            .foregroundStyle(Theme.Colors.ink)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: Theme.Space.m)
                        if let since = option.since {
                            Text(since)
                                .font(Theme.Text.meta)
                                .foregroundStyle(Theme.Colors.faint)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(Theme.Space.s)
                    .background(
                        Theme.State.needsYou.tint,
                        in: .rect(cornerRadius: Theme.Radius.control)
                    )
                }
                .buttonStyle(.plain)
                .help("Keep this one and retire the other")
            }
        }
        .padding(Theme.Space.m)
        .background(Theme.Colors.raised, in: .rect(cornerRadius: Theme.Radius.control))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.control)
                .stroke(Theme.State.needsYou.color.opacity(0.4), lineWidth: 1)
        )
    }
}

/// A text control that reads as a link rather than a button, for row-level actions.
private struct Quiet: View {
    let title: String
    let action: () -> Void

    init(_ title: String, action: @escaping () -> Void) {
        self.title = title
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.Colors.faint)
    }
}

private struct SearchField: View {
    @Binding var text: String

    var body: some View {
        HStack(spacing: Theme.Space.s) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11))
                .foregroundStyle(Theme.Colors.faint)
            TextField("Search what Loki knows", text: $text)
                .textFieldStyle(.plain)
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.ink)
        }
        .padding(.horizontal, Theme.Space.m)
        .padding(.vertical, Theme.Space.s)
        .background(Theme.Colors.canvas, in: .rect(cornerRadius: Theme.Radius.control))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.control)
                .stroke(Theme.Colors.line, lineWidth: 1)
        )
    }
}

/// Reveals the file a row came from, which is the answer to the summary-versus-reality gap.
private func reveal(_ path: String) {
    let root = FileManager.default
        .homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/Loki/memory")
    NSWorkspace.shared.activateFileViewerSelecting([root.appendingPathComponent(path)])
}

extension KnownEntity {
    /// Whether anything here mentions `needle`.
    ///
    /// Matching entities keep all their rows rather than being narrowed to the hit. §10.8 makes
    /// the same argument about reading a line range: a claim answers a question, and the block it
    /// sits in is what makes the answer checkable.
    ///
    /// Filtered in the app rather than in a query, because §10.7 puts this at 50 to 300 entities
    /// and a round trip per keystroke would buy nothing at that size.
    func mentions(_ needle: String) -> Bool {
        if name.lowercased().contains(needle) { return true }
        if facts.contains(where: {
            $0.text.lowercased().contains(needle) || $0.attribute.contains(needle)
        }) {
            return true
        }
        return questions.contains { question in
            question.attribute.contains(needle)
                || question.options.contains { $0.text.lowercased().contains(needle) }
        }
    }
}
