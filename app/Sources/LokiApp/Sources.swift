import SwiftUI

/// A page an answer was built from (§12.7).
///
/// **The favicon is bytes, not a URL.** Fetching one from the interface would open a second way out
/// of the process, which §21.7 forbids, and fetching it from a favicon service would tell that
/// service every site the user reads, which is the opposite of what this product promises. The page
/// was already fetched through the exit, its `<link rel="icon">` is in the HTML that came back, and
/// the icon rides in `evidence/` with it. By the time a citation is drawn there is nothing left to
/// fetch.
struct Source: Identifiable, Hashable {
    let id: Int
    /// The page a person can open. Never an internal endpoint the content happened to come from.
    let url: String
    let title: String
    /// The span of the page this claim came from, for the preview.
    let excerpt: String
    /// PNG or ICO bytes from `evidence/`, absent when the page offered none.
    let icon: Data?
    /// When it was read, since evidence ages (§12.7).
    let fetched: Date

    /// `example.com` from `https://example.com/a/b?c`. What a citation shows when it has no icon
    /// worth showing, and what the preview is titled with.
    var host: String {
        let stripped = url
            .replacingOccurrences(of: "https://", with: "")
            .replacingOccurrences(of: "http://", with: "")
        let authority = stripped.split(separator: "/").first.map(String.init) ?? stripped
        return authority.hasPrefix("www.") ? String(authority.dropFirst(4)) : authority
    }

    /// The letter a fallback mark carries. First letter of the registrable name, not of `www`.
    var initial: String {
        host.first.map { String($0).uppercased() } ?? "?"
    }
}

/// The favicon, a letter, or a globe, in that order.
///
/// Three levels rather than two, because the gap between them is the whole point: a real icon is
/// recognisable at 14pt, a letter in the accent is still specific to that site, and a globe says
/// only "somewhere on the web". Falling straight from icon to globe throws away the one cheap thing
/// that still distinguishes two sources.
struct SourceMark: View {
    let source: Source
    var size: CGFloat = 14

    var body: some View {
        Group {
            if let icon = source.icon, let image = NSImage(data: icon) {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
            } else if source.initial != "?" {
                Text(source.initial)
                    .font(.system(size: size * 0.62, weight: .semibold))
                    .foregroundStyle(Theme.Colors.onYellow)
                    .frame(width: size, height: size)
                    .background(Theme.Colors.yellow, in: .rect(cornerRadius: size * 0.28))
            } else {
                Image(systemName: "globe")
                    .font(.system(size: size * 0.78))
                    .foregroundStyle(Theme.Colors.tertiary)
            }
        }
        .frame(width: size, height: size)
        .clipShape(.rect(cornerRadius: size * 0.28))
    }
}

/// What a source looks like when the pointer rests on a citation.
///
/// Small on purpose. It answers "is this worth clicking", which needs the site, the headline and a
/// sentence, and nothing else. A preview that reproduces the page is a page.
struct SourcePreview: View {
    let source: Source

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            HStack(spacing: Theme.Space.s) {
                SourceMark(source: source, size: 16)
                Text(source.host)
                    .font(Theme.Text.meta)
                    .foregroundStyle(Theme.Colors.secondary)
                Spacer(minLength: 0)
                Text(source.fetched, format: .relative(presentation: .numeric))
                    .font(Theme.Text.micro)
                    .foregroundStyle(Theme.Colors.tertiary)
            }
            Text(source.title)
                .font(Theme.Text.bodyStrong)
                .foregroundStyle(Theme.Colors.primary)
                .lineLimit(2)
            if !source.excerpt.isEmpty {
                Text(source.excerpt)
                    .font(Theme.Text.meta)
                    .lineSpacing(3)
                    .foregroundStyle(Theme.Colors.secondary)
                    .lineLimit(3)
            }
        }
        .padding(Theme.Space.m)
        .frame(width: 268, alignment: .leading)
        .background(Theme.Colors.surfaceAlt, in: .rect(cornerRadius: Theme.Radius.panel))
        .overlay {
            RoundedRectangle(cornerRadius: Theme.Radius.panel)
                .strokeBorder(Theme.Colors.border, lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.45), radius: 18, y: 6)
    }
}

/// A citation inside a sentence (§12.7).
///
/// **It reads as punctuation, not as a control.** A claim carries its source, and a source that
/// interrupts the sentence to announce itself costs more attention than it returns. So: the site's
/// own mark, the host, and a count when a claim rests on more than one, at the size of the
/// surrounding text rather than larger.
struct InlineCitation: View {
    let sources: [Source]
    var onOpen: (Source) -> Void = { _ in }

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var hovering = false

    private var lead: Source? { sources.first }

    var body: some View {
        if let lead {
            Button { onOpen(lead) } label: {
                HStack(spacing: 4) {
                    SourceMark(source: lead, size: 13)
                    Text(lead.host)
                        .font(Theme.Text.micro)
                        .foregroundStyle(hovering ? Theme.Colors.primary : Theme.Colors.secondary)
                    if sources.count > 1 {
                        Text("+\(sources.count - 1)")
                            .font(Theme.Text.micro)
                            .foregroundStyle(Theme.Colors.tertiary)
                    }
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(
                    hovering ? Theme.Colors.surfaceAlt : Theme.Colors.surface,
                    in: .capsule
                )
                .overlay {
                    Capsule().strokeBorder(
                        hovering ? Theme.Colors.borderStrong : Theme.Colors.border,
                        lineWidth: 1
                    )
                }
                .contentShape(.capsule)
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .animation(reduceMotion ? nil : Theme.Motion.control, value: hovering)
            .popover(isPresented: $hovering, arrowEdge: .bottom) {
                SourcePreview(source: lead)
                    .padding(Theme.Space.xs)
            }
            .help(lead.title)
            .accessibilityLabel("Source: \(lead.host). \(lead.title)")
        }
    }
}

/// Every source an answer used, as one control.
///
/// **Overlapped rather than listed.** A row of separate marks reads as a set of things to consider
/// one by one; overlapping them makes one object that says how many there were, which is the only
/// thing worth saying before somebody asks.
struct SourceStack: View {
    let sources: [Source]
    var onOpen: () -> Void = {}

    @Environment(\.reduceMotion) private var reduceMotion
    @State private var hovering = false

    /// Beyond this the stack stops being a shape and starts being a queue.
    private static let shown = 4

    var body: some View {
        if !sources.isEmpty {
            Button(action: onOpen) {
                HStack(spacing: Theme.Space.s) {
                    HStack(spacing: -6) {
                        ForEach(Array(sources.prefix(Self.shown).enumerated()), id: \.element.id) {
                            index, source in
                            SourceMark(source: source, size: 18)
                                .overlay {
                                    RoundedRectangle(cornerRadius: 18 * 0.28)
                                        .strokeBorder(Theme.Colors.background, lineWidth: 1.5)
                                }
                                .zIndex(Double(Self.shown - index))
                                // Fans out under the pointer, so the stack shows what it is made
                                // of before it is clicked.
                                .offset(x: hovering ? CGFloat(index) * 3 : 0)
                        }
                    }
                    Text(sources.count == 1 ? "1 source" : "\(sources.count) sources")
                        .font(Theme.Text.meta)
                        .foregroundStyle(hovering ? Theme.Colors.primary : Theme.Colors.secondary)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(Theme.Colors.tertiary)
                        .offset(x: hovering ? 2 : 0)
                }
                .padding(.horizontal, Theme.Space.s)
                .padding(.vertical, 5)
                .background(
                    hovering ? Theme.Colors.surface : .clear,
                    in: .rect(cornerRadius: Theme.Radius.control)
                )
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .animation(reduceMotion ? nil : Theme.Motion.control, value: hovering)
            .accessibilityLabel("\(sources.count) sources. Opens the list.")
        }
    }
}

/// The sources of one answer, listed.
///
/// Lives in the right rail rather than in the thread, because it is a reference and the thread is a
/// record. Opening it is the one click the stack promises.
struct SourceList: View {
    let sources: [Source]
    var onOpen: (Source) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(sources) { source in
                SourceRow(source: source) { onOpen(source) }
                if source.id != sources.last?.id {
                    Divider().overlay(Theme.Colors.border)
                }
            }
        }
    }
}

private struct SourceRow: View {
    let source: Source
    let open: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: open) {
            HStack(alignment: .top, spacing: Theme.Space.s) {
                SourceMark(source: source, size: 16)
                    .padding(.top, 1)
                VStack(alignment: .leading, spacing: 2) {
                    Text(source.title)
                        .font(Theme.Text.body)
                        .foregroundStyle(Theme.Colors.primary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    Text(source.host)
                        .font(Theme.Text.micro)
                        .foregroundStyle(Theme.Colors.tertiary)
                }
                Spacer(minLength: 0)
                Image(systemName: "arrow.up.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(hovering ? Theme.Colors.secondary : .clear)
            }
            .padding(.vertical, Theme.Space.s)
            .padding(.horizontal, Theme.Space.s)
            .background(hovering ? Theme.Colors.surface : .clear)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(Theme.Motion.control, value: hovering)
    }
}
