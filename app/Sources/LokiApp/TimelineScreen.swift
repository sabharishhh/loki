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
    @State private var duplicates: [Duplicate] = []
    @State private var search = ""
    @State private var loading = true

    private var shown: [KnownEntity] {
        let needle = search.trimmingCharacters(in: .whitespaces).lowercased()
        guard !needle.isEmpty else { return entities }
        return entities.filter { $0.mentions(needle) }
    }

    /// Facts with something else said about them. Worth a look, never a blocker.
    private var worthChecking: Int {
        entities.reduce(0) { $0 + $1.facts.filter { !$0.alsoSaid.isEmpty }.count }
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
                    // Above the cards, because it is about the shape of the list rather than about
                    // any one row, and because a split nobody sees is worse than one nobody fixes.
                    ForEach(duplicates) { split in
                        SplitCard(split: split, conversation: conversation, reload: reload)
                    }
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
        .background(Theme.Colors.surface)
        .task {
            let known = conversation.knowledge()
            entities = known.entities
            duplicates = known.duplicates
            withAnimation(Theme.Motion.control) { loading = false }
        }
    }

    private func reload() {
        let known = conversation.knowledge()
        withAnimation(Theme.Motion.control) {
            entities = known.entities
            duplicates = known.duplicates
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            VStack(alignment: .leading, spacing: Theme.Space.xs) {
                Text("What Loki knows")
                    .font(Theme.Text.display)
                    .kerning(Theme.Text.displayTracking)
                    .foregroundStyle(Theme.Colors.primary)
                Text(subtitle)
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.secondary)
            }

            // Search appears once there is enough here to need it. Before that it is a control
            // asking to be used on four rows, which is noise.
            if entities.count > 4 {
                SearchField(text: $search)
            }
        }
    }

    private var subtitle: String {
        if worthChecking == 1 {
            return "One thing is worth checking. Every row is a line in a file you can open."
        }
        if worthChecking > 1 {
            return "\(worthChecking) things are worth checking. "
                + "Every row is a line in a file you can open."
        }
        return "Grouped by what it is about. Every row is a line in a file you can open."
    }
}

/// Two cards that answer to one name, and the one tap that folds them together.
///
/// Phrased as a question, not as an error. The store cannot know whether they are the same person,
/// and a wrong merge hides a true fact where a wrong split only leaves two rows, so the default is
/// to leave them alone.
private struct SplitCard: View {
    let split: Duplicate
    let conversation: Conversation
    let reload: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Text("Two entries answer to \"\(split.form)\"")
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.primary)

            ForEach(Array(split.paths.enumerated()), id: \.element) { at, path in
                Text("\(split.names[at])  ·  \(path)")
                    .font(Theme.Text.micro)
                    .foregroundStyle(Theme.Colors.secondary)
            }

            HStack(spacing: Theme.Space.m) {
                Quiet("same thing, join them") {
                    // Into the fuller card, which the core puts first.
                    guard let into = split.paths.first, split.paths.count > 1 else { return }
                    for path in split.paths.dropFirst() {
                        conversation.merge(from: path, into: into)
                    }
                    reload()
                }
                Text("or leave them, if they are different")
                    .font(Theme.Text.micro)
                    .foregroundStyle(Theme.Colors.secondary)
            }
        }
        .padding(Theme.Space.m)
        .background(Theme.State.thinking.tint, in: .rect(cornerRadius: Theme.Radius.panel))
        .frame(maxWidth: .infinity, alignment: .leading)
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

            ForEach(entity.facts) { fact in
                FactRow(entity: entity, fact: fact, conversation: conversation, reload: reload)
            }

            // Names and edges are knowledge and belong on the page. They lived only in the file
            // until now, so being told a nickname three times looked exactly like being ignored.
            if !entity.alsoKnownAs.isEmpty || !entity.relations.isEmpty {
                Names(entity: entity, conversation: conversation, reload: reload)
            }
        }
        .padding(Theme.Space.l)
        .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.panel))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.panel)
                .stroke(Theme.Colors.border, lineWidth: 1)
        )
        .opacity(appeared ? 1 : 0)
        .offset(y: appeared ? 0 : 6)
        .onAppear { withAnimation(Theme.Motion.control) { appeared = true } }
    }

    private var heading: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s) {
            Text(entity.name)
                .font(Theme.Text.title)
                .kerning(Theme.Text.titleTracking)
                .foregroundStyle(Theme.Colors.primary)

            Text(entity.kind)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)

            if entity.confirmed {
                Text("you confirmed this")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.State.idle.color)
            } else if !entity.inUse {
                // The consequence, not the state name: what matters is that Loki is holding this
                // back, not that a field somewhere says `draft`.
                Text("not in use yet")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.State.thinking.color)
            }

            Spacer(minLength: 0)

            Button { reveal(entity.path) } label: {
                Text("open file")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.Colors.tertiary)
            .help("Show \(entity.path) in Finder")
        }
    }
}

/// One fact, with what it replaced folded onto the same row.
///
/// §5's rule that colour means machine state holds here, so the emotional centre of the product
/// gets no hue at all and is carried by typography.
/// What an entity is called, and who it is connected to.
///
/// Quieter than a fact row, because these are how Loki finds things rather than things it believes.
/// Still removable: an alias goes, and an edge closes, because a manager who changed is a different
/// thing from a manager who was never yours.
private struct Names: View {
    let entity: KnownEntity
    let conversation: Conversation
    let reload: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            if !entity.alsoKnownAs.isEmpty {
                row("also called", entity.alsoKnownAs.map { form in
                    (form, { conversation.forgetAlias(path: entity.path, form: form) })
                })
            }
            if !entity.relations.isEmpty {
                row("connected to", entity.relations.map { edge in
                    ("\(edge.name), \(edge.label)", {
                        conversation.forgetRelation(
                            path: entity.path, label: edge.label, to: edge.path
                        )
                    })
                })
            }
        }
        .padding(.top, Theme.Space.xs)
    }

    private func row(_ label: String, _ items: [(String, () -> Void)]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)
            ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                HStack(spacing: Theme.Space.s) {
                    Text(item.0)
                        .font(Theme.Text.body)
                        .foregroundStyle(Theme.Colors.secondary)
                    Spacer(minLength: Theme.Space.m)
                    Quiet("not true") {
                        item.1()
                        reload()
                    }
                }
            }
        }
    }
}

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
                .fill(Theme.Colors.border)
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
                        .foregroundStyle(Theme.Colors.primary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if let since = fact.since {
                    Text(since)
                        .font(Theme.Text.meta)
                        .kerning(Theme.Text.metaTracking)
                        .foregroundStyle(Theme.Colors.tertiary)
                }

                if let was = fact.was {
                    correction(was)
                }

                if !fact.alsoSaid.isEmpty {
                    alternatives
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
                    .foregroundStyle(Theme.Colors.tertiary)
            }
            if fact.fromElsewhere {
                Text("I read this somewhere")
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.State.thinking.color)
            }
        }
    }

    /// The superseded half. Quieter than the live one, and it says how long Loki was wrong.
    private func correction(_ was: Superseded) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(was.text)
                .font(Theme.Text.body)
                .strikethrough(true, color: Theme.Colors.tertiary)
                .foregroundStyle(Theme.Colors.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(wrongLine(was))
                .font(Theme.Text.meta)
                .kerning(Theme.Text.metaTracking)
                .foregroundStyle(Theme.Colors.tertiary)
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
                .foregroundStyle(Theme.Colors.primary)
                .padding(Theme.Space.s)
                .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.control))

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

    /// What else was said about this property, and the one tap that settles it.
    ///
    /// Rule 4 no longer blocks: Loki uses the later statement and keeps this here to be checked.
    /// An approval queue nobody works through is worse than a decision the user can see and flip.
    private var alternatives: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            Text("You also said")
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.State.thinking.color)

            ForEach(fact.alsoSaid) { other in
                HStack(alignment: .top, spacing: Theme.Space.s) {
                    Text(other.text)
                        .font(Theme.Text.body)
                        .foregroundStyle(Theme.Colors.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: Theme.Space.m)
                    Quiet("use this instead") {
                        conversation.settle(path: entity.path, keep: other.ordinal)
                        reload()
                    }
                }
            }
        }
        .padding(Theme.Space.s)
        .background(Theme.State.thinking.tint, in: .rect(cornerRadius: Theme.Radius.control))
        .padding(.top, Theme.Space.xs)
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
            if !fact.alsoSaid.isEmpty {
                Quiet("this one is right") {
                    conversation.settle(path: entity.path, keep: fact.ordinal)
                    reload()
                }
            }
        }
        .padding(.top, Theme.Space.xs)
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
        .foregroundStyle(Theme.Colors.tertiary)
    }
}

private struct SearchField: View {
    @Binding var text: String

    var body: some View {
        HStack(spacing: Theme.Space.s) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11))
                .foregroundStyle(Theme.Colors.tertiary)
            TextField("Search what Loki knows", text: $text)
                .textFieldStyle(.plain)
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.primary)
        }
        .padding(.horizontal, Theme.Space.m)
        .padding(.vertical, Theme.Space.s)
        .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.control))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.control)
                .stroke(Theme.Colors.border, lineWidth: 1)
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
        return facts.contains { fact in
            fact.text.lowercased().contains(needle)
                || fact.attribute.contains(needle)
                || fact.alsoSaid.contains { $0.text.lowercased().contains(needle) }
        }
    }
}
