import SwiftUI

/// Markdown in an assistant response, per §9.10 of the design system.
///
/// The parse is a pure function of the text so far, which is what makes it safe to run on every
/// token. Blocks already emitted keep their shape as more arrives; only the last one changes.
struct MarkdownText: View {
    let text: String

    var body: some View {
        Blocks(blocks: Markdown.parse(text))
            .textSelection(.enabled)
    }
}

/// A run of blocks. Recursive, because a list item can hold blocks of its own.
private struct Blocks: View {
    let blocks: [Markdown.Block]

    /// Position and kind together. A block that changes kind at the same position is a different
    /// view, which is what stops SwiftUI reusing one across the change.
    private var placed: [Placed] {
        blocks.enumerated().map { Placed(id: "\($0.offset).\($0.element.kind)", block: $0.element) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            // Position plus kind, not position alone. The stream is append-only so a block's
            // position is stable, but the *last* block changes kind as it grows: a paragraph
            // becomes a list the moment "- " arrives. Identity by position alone reuses the view
            // across that change and it can keep stale layout.
            ForEach(placed) { placed in
                BlockView(block: placed.block)
            }
        }
    }
}

private struct Placed: Identifiable {
    let id: String
    let block: Markdown.Block
}

private struct BlockView: View {
    let block: Markdown.Block

    var body: some View {
        switch block {
        case let .heading(level, text):
            Text(Markdown.inline(text, base: Theme.Text.heading(level)))
                .foregroundStyle(Theme.Colors.primary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, level <= 2 ? Theme.Space.s : 0)

        case let .paragraph(text):
            Text(Markdown.inline(text, base: Theme.Text.record))
                .lineSpacing(Theme.Text.recordLineSpacing)
                .foregroundStyle(Theme.Colors.primary)
                .fixedSize(horizontal: false, vertical: true)

        case let .code(language, lines):
            CodeBlock(language: language, lines: lines)

        case let .list(items, ordered, start):
            VStack(alignment: .leading, spacing: Theme.Space.s) {
                ForEach(Array(items.enumerated()), id: \.offset) { at, item in
                    ListRow(item: item, marker: ordered ? "\(start + at)." : "•")
                }
            }

        case let .quote(blocks):
            HStack(alignment: .top, spacing: Theme.Space.m) {
                Rectangle()
                    .fill(Theme.Colors.border)
                    .frame(width: Theme.Size.rail)
                Blocks(blocks: blocks)
                    .foregroundStyle(Theme.Colors.secondary)
            }

        case let .table(header, rows, alignments):
            TableBlock(header: header, rows: rows, alignments: alignments)

        case .rule:
            Rectangle()
                .fill(Theme.Colors.border)
                .frame(height: 1)
                .padding(.vertical, Theme.Space.s)
        }
    }
}

private struct ListRow: View {
    let item: Markdown.Item
    let marker: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s) {
            Group {
                if let done = item.checked {
                    Image(systemName: done ? "checkmark.square.fill" : "square")
                        .font(.system(size: 11))
                        .foregroundStyle(done ? Theme.State.idle.color : Theme.Colors.tertiary)
                } else {
                    Text(marker)
                        .font(Theme.Text.record)
                        .foregroundStyle(Theme.Colors.tertiary)
                        .monospacedDigit()
                }
            }
            .frame(minWidth: 18, alignment: .trailing)

            VStack(alignment: .leading, spacing: Theme.Space.s) {
                Text(Markdown.inline(item.text, base: Theme.Text.record))
                    .lineSpacing(Theme.Text.recordLineSpacing)
                    .foregroundStyle(Theme.Colors.primary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if !item.children.isEmpty {
                    Blocks(blocks: item.children)
                }
            }
        }
    }
}

/// A fenced block. Scrolls sideways in its own well rather than widening the column.
private struct CodeBlock: View {
    let language: String?
    let lines: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let language, !language.isEmpty {
                Text(language)
                    .font(Theme.Text.micro)
                    .kerning(Theme.Text.microTracking)
                    .foregroundStyle(Theme.Colors.tertiary)
                    .padding(.horizontal, Theme.Space.m)
                    .padding(.top, Theme.Space.s)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                Text(Syntax.highlight(lines.joined(separator: "\n"), language: language))
                    .textSelection(.enabled)
                    .padding(Theme.Space.m)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.control))
    }
}

/// A table. Scrolls inside its own well rather than forcing the column wider (§9.10).
private struct TableBlock: View {
    let header: [String]
    let rows: [[String]]
    let alignments: [Markdown.Alignment]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                row(header, isHeader: true)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, cells in
                    Rectangle().fill(Theme.Colors.border).frame(height: 1)
                    row(cells, isHeader: false)
                }
            }
        }
        .background(Theme.Colors.background, in: .rect(cornerRadius: Theme.Radius.control))
    }

    private func alignment(_ column: Int) -> SwiftUI.Alignment {
        switch alignments.indices.contains(column) ? alignments[column] : .leading {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }

    private func row(_ cells: [String], isHeader: Bool) -> some View {
        HStack(alignment: .top, spacing: Theme.Space.l) {
            ForEach(Array(cells.enumerated()), id: \.offset) { column, cell in
                Text(Markdown.inline(cell, base: isHeader ? Theme.Text.bodyStrong : Theme.Text.body))
                    .foregroundStyle(isHeader ? Theme.Colors.primary : Theme.Colors.secondary)
                    .frame(minWidth: 80, alignment: alignment(column))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, Theme.Space.m)
        .padding(.vertical, Theme.Space.s)
    }
}

/// The parser. Line-based, recursive for nesting, and total: any input produces blocks.
enum Markdown {
    enum Alignment: Equatable {
        case leading, center, trailing
    }

    struct Item: Equatable {
        let text: String
        /// Nil for an ordinary bullet, otherwise a task list checkbox.
        let checked: Bool?
        /// Nested lists, code, or further paragraphs indented under this item.
        let children: [Block]
    }

    indirect enum Block: Equatable {
        case heading(level: Int, text: String)
        case paragraph(String)
        case code(language: String?, lines: [String])
        case list(items: [Item], ordered: Bool, start: Int)
        case quote([Block])
        case table(header: [String], rows: [[String]], alignments: [Alignment])
        case rule

        /// Which shape this is, for view identity. Not the content: only a change of kind should
        /// break identity, or every token would rebuild the block it is growing.
        var kind: String {
            switch self {
            case .heading: "heading"
            case .paragraph: "paragraph"
            case .code: "code"
            case .list: "list"
            case .quote: "quote"
            case .table: "table"
            case .rule: "rule"
            }
        }
    }

    static func parse(_ text: String) -> [Block] {
        parse(lines: text.components(separatedBy: .newlines))
    }

    private static func parse(lines input: [String]) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var lines = input[...]

        func flushParagraph() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.joined(separator: "\n")))
                paragraph.removeAll()
            }
        }

        while let line = lines.first {
            lines = lines.dropFirst()
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if let language = fenceLanguage(trimmed) {
                flushParagraph()
                var body: [String] = []
                // An unterminated fence still renders as a code block, immediately. Prose that
                // snaps into a block when the closing fence lands is worse than a block that
                // grows (§9.10).
                while let next = lines.first,
                      fenceLanguage(next.trimmingCharacters(in: .whitespaces)) == nil {
                    body.append(next)
                    lines = lines.dropFirst()
                }
                if lines.first != nil { lines = lines.dropFirst() }
                blocks.append(.code(language: language, lines: dedent(body)))
                continue
            }

            if trimmed.isEmpty {
                flushParagraph()
                continue
            }

            // A run of = or - directly under a paragraph is a setext heading, not a rule.
            if !paragraph.isEmpty, let level = setextLevel(trimmed) {
                let text = paragraph.joined(separator: " ")
                paragraph.removeAll()
                blocks.append(.heading(level: level, text: text))
                continue
            }

            if isRule(trimmed) {
                flushParagraph()
                blocks.append(.rule)
                continue
            }

            if let (level, body) = atxHeading(trimmed) {
                flushParagraph()
                blocks.append(.heading(level: level, text: body))
                continue
            }

            if trimmed.hasPrefix(">") {
                flushParagraph()
                var quoted = [strip(trimmed, ">")]
                while let next = lines.first?.trimmingCharacters(in: .whitespaces),
                      next.hasPrefix(">") {
                    quoted.append(strip(next, ">"))
                    lines = lines.dropFirst()
                }
                // Recursive, so a quote can hold lists, code, or a nested quote.
                blocks.append(.quote(parse(lines: quoted)))
                continue
            }

            if isTableRow(trimmed), let next = lines.first?.trimmingCharacters(in: .whitespaces),
               isTableDivider(next) {
                flushParagraph()
                let alignments = columnAlignments(next)
                lines = lines.dropFirst()
                var rows: [[String]] = []
                while let candidate = lines.first?.trimmingCharacters(in: .whitespaces),
                      isTableRow(candidate) {
                    rows.append(cells(candidate))
                    lines = lines.dropFirst()
                }
                blocks.append(.table(header: cells(trimmed), rows: rows, alignments: alignments))
                continue
            }

            if let first = listItem(trimmed) {
                flushParagraph()
                var gathered = [line]
                // A list runs until a line that is neither an item nor indented under one. Blank
                // lines are kept so a loose list can hold paragraphs.
                while let next = lines.first {
                    let candidate = next.trimmingCharacters(in: .whitespaces)
                    if candidate.isEmpty {
                        guard let after = lines.dropFirst().first,
                              indent(after) > 0 || listItem(after.trimmingCharacters(in: .whitespaces)) != nil
                        else { break }
                        gathered.append(next)
                        lines = lines.dropFirst()
                        continue
                    }
                    guard listItem(candidate) != nil || indent(next) > indent(line) else { break }
                    gathered.append(next)
                    lines = lines.dropFirst()
                }
                blocks.append(buildList(gathered, ordered: first.ordered, start: first.start))
                continue
            }

            paragraph.append(trimmed)
        }
        flushParagraph()
        return blocks
    }

    /// Splits a gathered run into items, and parses each item's indented continuation recursively.
    private static func buildList(_ lines: [String], ordered: Bool, start: Int) -> Block {
        var items: [Item] = []
        var current: (head: Item, body: [String])?

        func close() {
            guard let open = current else { return }
            let children = open.body.isEmpty ? [] : parse(lines: dedent(open.body))
            items.append(Item(text: open.head.text, checked: open.head.checked, children: children))
            current = nil
        }

        for line in lines {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            // Only an item at the outer indent starts a sibling. A deeper one belongs to the
            // item above it and is parsed as a nested list.
            if indent(line) == 0, let parsed = listItem(trimmed) {
                close()
                current = (parsed.item, [])
                continue
            }
            current?.body.append(line)
        }
        close()
        return .list(items: items, ordered: ordered, start: start)
    }

    /// Renders one span of inline markdown, falling back to the raw text if it will not parse.
    ///
    /// Partial emphasis is normal mid-stream, so a failure here is expected rather than a fault.
    static func inline(_ text: String, base: Font) -> AttributedString {
        let source = autolinked(hardBreaks(text))
        var out: AttributedString
        if let parsed = try? AttributedString(
            markdown: source,
            options: .init(
                allowsExtendedAttributes: true,
                interpretedSyntax: .inlineOnlyPreservingWhitespace,
                failurePolicy: .returnPartiallyParsedIfPossible
            )
        ) {
            out = parsed
        } else {
            out = AttributedString(text)
        }
        out.font = base
        for run in out.runs {
            if run.inlinePresentationIntent?.contains(.code) == true {
                out[run.range].font = Theme.Text.code
                out[run.range].backgroundColor = Theme.Colors.background
            }
            if run.inlinePresentationIntent?.contains(.strikethrough) == true {
                out[run.range].strikethroughStyle = .single
                out[run.range].foregroundColor = Theme.Colors.secondary
            }
            if run.link != nil {
                out[run.range].foregroundColor = Theme.State.reading.color
                out[run.range].underlineStyle = .single
            }
        }
        return out
    }

    /// Two trailing spaces or a trailing backslash mean a line break inside a paragraph.
    private static func hardBreaks(_ text: String) -> String {
        text.components(separatedBy: "\n")
            .map { line -> String in
                if line.hasSuffix("  ") || line.hasSuffix("\\") {
                    return line.hasSuffix("\\")
                        ? String(line.dropLast())
                        : line.trimmingCharacters(in: .whitespaces)
                }
                return line
            }
            .joined(separator: "\n")
    }

    /// Turns a bare URL into a link. Skips anything already inside markdown link syntax.
    private static func autolinked(_ text: String) -> String {
        guard text.contains("http") else { return text }
        guard let regex = try? NSRegularExpression(
            pattern: "(?<![\\(\\[<])\\bhttps?://[^\\s<>\\)\\]]+"
        ) else { return text }
        let range = NSRange(text.startIndex..., in: text)
        return regex.stringByReplacingMatches(in: text, range: range, withTemplate: "<$0>")
    }

    private static func fenceLanguage(_ line: String) -> String? {
        guard line.hasPrefix("```") || line.hasPrefix("~~~") else { return nil }
        return String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
    }

    private static func isRule(_ line: String) -> Bool {
        let stripped = line.replacingOccurrences(of: " ", with: "")
        guard stripped.count >= 3 else { return false }
        return stripped.allSatisfy { $0 == "-" }
            || stripped.allSatisfy { $0 == "*" }
            || stripped.allSatisfy { $0 == "_" }
    }

    private static func setextLevel(_ line: String) -> Int? {
        guard line.count >= 2 else { return nil }
        if line.allSatisfy({ $0 == "=" }) { return 1 }
        if line.count >= 3, line.allSatisfy({ $0 == "-" }) { return 2 }
        return nil
    }

    private static func atxHeading(_ line: String) -> (Int, String)? {
        guard line.hasPrefix("#") else { return nil }
        let hashes = line.prefix { $0 == "#" }.count
        guard hashes <= 6, line.dropFirst(hashes).hasPrefix(" ") else { return nil }
        let body = String(line.dropFirst(hashes))
            .trimmingCharacters(in: .whitespaces)
            .trimmingCharacters(in: CharacterSet(charactersIn: "#"))
            .trimmingCharacters(in: .whitespaces)
        return (hashes, body)
    }

    private static func listItem(_ line: String) -> (item: Item, ordered: Bool, start: Int)? {
        for bullet in ["- ", "* ", "+ "] where line.hasPrefix(bullet) {
            let body = String(line.dropFirst(bullet.count))
            if body.hasPrefix("[ ] ") {
                return (Item(text: String(body.dropFirst(4)), checked: false, children: []), false, 1)
            }
            if body.lowercased().hasPrefix("[x] ") {
                return (Item(text: String(body.dropFirst(4)), checked: true, children: []), false, 1)
            }
            return (Item(text: body, checked: nil, children: []), false, 1)
        }
        let digits = line.prefix { $0.isNumber }
        if !digits.isEmpty {
            let rest = line.dropFirst(digits.count)
            for separator in [". ", ") "] where rest.hasPrefix(separator) {
                let body = String(rest.dropFirst(separator.count))
                return (Item(text: body, checked: nil, children: []), true, Int(digits) ?? 1)
            }
        }
        return nil
    }

    private static func indent(_ line: String) -> Int {
        // A tab is four columns, which is what every generator this will see emits.
        line.prefix { $0 == " " || $0 == "\t" }
            .reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
    }

    /// Removes the smallest common indent, so a nested run parses as if it were top level.
    private static func dedent(_ lines: [String]) -> [String] {
        let widths = lines.filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }.map(indent)
        guard let smallest = widths.min(), smallest > 0 else { return lines }
        return lines.map { line in
            var dropped = 0
            var out = Substring(line)
            while dropped < smallest, let first = out.first, first == " " || first == "\t" {
                dropped += first == "\t" ? 4 : 1
                out = out.dropFirst()
            }
            return String(out)
        }
    }

    private static func isTableRow(_ line: String) -> Bool {
        line.hasPrefix("|") && line.dropFirst().contains("|")
    }

    private static func isTableDivider(_ line: String) -> Bool {
        isTableRow(line)
            && cells(line).allSatisfy { cell in
                !cell.isEmpty && cell.allSatisfy { $0 == "-" || $0 == ":" || $0 == " " }
            }
    }

    private static func columnAlignments(_ divider: String) -> [Alignment] {
        cells(divider).map { cell in
            let left = cell.hasPrefix(":")
            let right = cell.hasSuffix(":")
            if left && right { return .center }
            if right { return .trailing }
            return .leading
        }
    }

    private static func cells(_ line: String) -> [String] {
        line.split(separator: "|", omittingEmptySubsequences: false)
            .dropFirst()
            .dropLast(line.hasSuffix("|") ? 1 : 0)
            .map { $0.trimmingCharacters(in: .whitespaces) }
    }

    private static func strip(_ line: String, _ prefix: String) -> String {
        String(line.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
    }
}
