import SwiftUI

/// The memory timeline (§17.3).
///
/// Every promotion and correction in plain language. This is the trust surface for the whole
/// product: correctness under the hood is not what a user feels, being able to check the work is.
///
/// The rows come from `log.md`, which is the record. Rendering from anywhere else would let the
/// screen and the file disagree, and `open file` is the answer to the summary-versus-reality gap.
struct TimelineScreen: View {
    let conversation: Conversation

    @State private var rows: [String] = []
    @State private var loading = true

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Space.l) {
                header

                if loading {
                    VStack(alignment: .leading, spacing: Theme.Space.m) {
                        ForEach(0..<4, id: \.self) { n in
                            Skeleton(width: [0.9, 0.7, 0.85, 0.6][n], height: 14)
                        }
                    }
                } else if rows.isEmpty {
                    Empty(
                        "Nothing learned yet",
                        detail: "Tell Loki something about yourself and it appears here, "
                            + "with what it replaced and how long it was wrong."
                    )
                } else {
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        TimelineRow(sentence: row)
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
            rows = conversation.timeline()
            withAnimation(Theme.Motion.standard) { loading = false }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            Text("What Loki has learned")
                .font(Theme.Text.display)
                .kerning(Theme.Text.displayTracking)
                .foregroundStyle(Theme.Colors.ink)
            Text("Newest first. Every row is a line in a file you can open.")
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.muted)
        }
        .padding(.bottom, Theme.Space.s)
    }
}

/// One row.
///
/// A correction is drawn as a pair: the superseded claim struck through in `--muted`, the live one
/// in `--ink`, and both date ranges in mono. §5's rule that colour means machine state holds here,
/// so the emotional centre of the product gets no hue at all and is carried by typography.
private struct TimelineRow: View {
    let sentence: String

    @State private var shown = false

    var body: some View {
        let parsed = Parsed(sentence)

        HStack(alignment: .top, spacing: Theme.Space.m) {
            Rectangle()
                .fill(parsed.accent)
                .frame(width: Theme.Size.rail)
                .frame(maxHeight: .infinity)

            VStack(alignment: .leading, spacing: Theme.Space.xs) {
                Text(parsed.kind)
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.faint)

                if let old = parsed.replaced {
                    Text(old)
                        .font(Theme.Text.record)
                        .strikethrough(true, color: Theme.Colors.faint)
                        .foregroundStyle(Theme.Colors.muted)
                        .fixedSize(horizontal: false, vertical: true)
                }

                Text(parsed.live)
                    .font(Theme.Text.record)
                    .lineSpacing(Theme.Text.recordLineSpacing)
                    .foregroundStyle(Theme.Colors.ink)
                    .fixedSize(horizontal: false, vertical: true)

                if let dates = parsed.dates {
                    Text(dates)
                        .font(Theme.Text.meta)
                        .kerning(Theme.Text.metaTracking)
                        .monospacedDigit()
                        .foregroundStyle(Theme.Colors.faint)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .fixedSize(horizontal: false, vertical: true)
        .opacity(shown ? 1 : 0)
        .offset(y: shown ? 0 : 6)
        .onAppear {
            withAnimation(Theme.Motion.standard) { shown = true }
        }
    }
}

/// Splits a `log.md` sentence into the parts the correction pair needs.
///
/// Tolerant: an unrecognised line still renders as itself. A timeline that hides a row it cannot
/// parse would be lying about the file it claims to reflect.
private struct Parsed {
    let kind: String
    let live: String
    let replaced: String?
    let dates: String?
    let accent: Color

    init(_ sentence: String) {
        let lower = sentence.lowercased()
        if lower.hasPrefix("corrected") {
            kind = "corrected"
            accent = Theme.Colors.line
        } else if lower.hasPrefix("needs you") {
            kind = "needs you"
            accent = Theme.State.needsYou.color
        } else {
            kind = "learned"
            accent = Theme.Colors.line
        }

        let body = sentence
            .drop(while: { $0 != ":" })
            .dropFirst()
            .trimmingCharacters(in: .whitespaces)

        let quoted = Parsed.quotes(in: body)
        live = quoted.first ?? body
        replaced = quoted.count > 1 ? quoted[1] : nil

        var pieces: [String] = []
        if let from = Parsed.after("from ", in: body) { pieces.append("from \(from)") }
        if let since = Parsed.after("held since ", in: body) { pieces.append("held since \(since)") }
        if let days = Parsed.after("wrong for ", in: body) { pieces.append("wrong for \(days)") }
        dates = pieces.isEmpty ? nil : pieces.joined(separator: "   ")
    }

    private static func quotes(in text: String) -> [String] {
        var out: [String] = []
        var current: String?
        for character in text {
            if character == "\"" {
                if let open = current {
                    out.append(open)
                    current = nil
                } else {
                    current = ""
                }
            } else if current != nil {
                current?.append(character)
            }
        }
        return out
    }

    private static func after(_ marker: String, in text: String) -> String? {
        guard let range = text.range(of: marker) else { return nil }
        let rest = text[range.upperBound...]
        let value = rest.prefix { $0 != "," && $0 != "\"" }
        let trimmed = value.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? nil : trimmed
    }
}
