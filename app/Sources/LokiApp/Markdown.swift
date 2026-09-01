import SwiftUI

/// Markdown in an assistant response, per §9.10 of the design system.
///
/// Supported: h2 and h3, bold, italic, inline code, fenced code, ordered and unordered lists,
/// task lists, tables, blockquotes, links, rules. Not supported: h1, because the screen owns the
/// top of the hierarchy, and raw HTML.
///
/// The parse is a pure function of the text so far, which is what makes it safe to run on every
/// token. Blocks already emitted keep their shape as more arrives; only the last one changes.
struct MarkdownText: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            // Positional identity is correct here and index identity is not the usual mistake:
            // the stream is append-only, so a block's position is stable for its whole life and
            // only the last one is still growing.
            ForEach(Array(Markdown.parse(text).enumerated()), id: \.offset) { _, block in
                BlockView(block: block)
            }
        }
        .textSelection(.enabled)
    }
}

private struct BlockView: View {
    let block: Markdown.Block

    var body: some View {
        switch block {
        case let .heading(level, text):
            Text(Markdown.inline(text, base: level == 2 ? Theme.Text.title : Theme.Text.subhead))
                .foregroundStyle(Theme.Colors.ink)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, Theme.Space.s)

        case let .paragraph(text):
            Text(Markdown.inline(text, base: Theme.Text.record))
                .lineSpacing(Theme.Text.recordLineSpacing)
                .foregroundStyle(Theme.Colors.ink)
                .fixedSize(horizontal: false, vertical: true)

        case let .code(language, lines):
            CodeBlock(language: language, lines: lines)

        case let .list(items, ordered):
            VStack(alignment: .leading, spacing: Theme.Space.s) {
                ForEach(Array(items.enumerated()), id: \.offset) { at, item in
                    ListRow(item: item, marker: ordered ? "\(at + 1)." : "•")
                }
            }

        case let .quote(lines):
            HStack(alignment: .top, spacing: Theme.Space.m) {
                Rectangle()
                    .fill(Theme.Colors.line)
                    .frame(width: Theme.Size.rail)
                Text(Markdown.inline(lines.joined(separator: " "), base: Theme.Text.record))
                    .lineSpacing(Theme.Text.recordLineSpacing)
                    .foregroundStyle(Theme.Colors.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }

        case let .table(header, rows):
            TableBlock(header: header, rows: rows)

        case .rule:
            Rectangle()
                .fill(Theme.Colors.line)
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
                        .foregroundStyle(done ? Theme.State.reading.color : Theme.Colors.faint)
                } else {
                    Text(marker)
                        .font(Theme.Text.record)
                        .foregroundStyle(Theme.Colors.faint)
                        .monospacedDigit()
                }
            }
            .frame(minWidth: 16, alignment: .trailing)

            Text(Markdown.inline(item.text, base: Theme.Text.record))
                .lineSpacing(Theme.Text.recordLineSpacing)
                .foregroundStyle(Theme.Colors.ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
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
                    .foregroundStyle(Theme.Colors.faint)
                    .padding(.horizontal, Theme.Space.m)
                    .padding(.top, Theme.Space.s)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                Text(lines.joined(separator: "\n"))
                    .font(Theme.Text.code)
                    .foregroundStyle(Theme.Colors.ink)
                    .textSelection(.enabled)
                    .padding(Theme.Space.m)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.control))
    }
}

/// A table. Scrolls inside its own well rather than forcing the column wider (§9.10).
private struct TableBlock: View {
    let header: [String]
    let rows: [[String]]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                row(header, isHeader: true)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, cells in
                    Rectangle().fill(Theme.Colors.line).frame(height: 1)
                    row(cells, isHeader: false)
                }
            }
        }
        .background(Theme.Colors.sunk, in: .rect(cornerRadius: Theme.Radius.control))
    }

    private func row(_ cells: [String], isHeader: Bool) -> some View {
        HStack(alignment: .top, spacing: Theme.Space.l) {
            ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                Text(Markdown.inline(cell, base: isHeader ? Theme.Text.bodyStrong : Theme.Text.body))
                    .foregroundStyle(isHeader ? Theme.Colors.ink : Theme.Colors.muted)
                    .frame(minWidth: 80, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, Theme.Space.m)
        .padding(.vertical, Theme.Space.s)
    }
}

/// The parser. Line-based, and total: any input produces blocks rather than an error.
enum Markdown {
    struct Item: Equatable {
        let text: String
        /// Nil for an ordinary bullet, otherwise a task list checkbox.
        let checked: Bool?
    }

    enum Block: Equatable {
        case heading(level: Int, text: String)
        case paragraph(String)
        case code(language: String?, lines: [String])
        case list(items: [Item], ordered: Bool)
        case quote([String])
        case table(header: [String], rows: [[String]])
        case rule
    }

    static func parse(_ text: String) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var lines = text.components(separatedBy: .newlines)[...]

        func flushParagraph() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.joined(separator: " ")))
                paragraph.removeAll()
            }
        }

        while let line = lines.first {
            lines = lines.dropFirst()
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if let fence = fenceLanguage(trimmed) {
                flushParagraph()
                var body: [String] = []
                // An unterminated fence still renders as a code block, immediately. Prose that
                // snaps into a block when the closing fence lands is worse than a block that
                // grows (§9.10).
                while let next = lines.first, fenceLanguage(next.trimmingCharacters(in: .whitespaces)) == nil {
                    body.append(next)
                    lines = lines.dropFirst()
                }
                if lines.first != nil { lines = lines.dropFirst() }
                blocks.append(.code(language: fence, lines: body))
                continue
            }

            if trimmed.isEmpty {
                flushParagraph()
                continue
            }

            if isRule(trimmed) {
                flushParagraph()
                blocks.append(.rule)
                continue
            }

            if let (level, body) = heading(trimmed) {
                flushParagraph()
                blocks.append(.heading(level: level, text: body))
                continue
            }

            if trimmed.hasPrefix(">") {
                flushParagraph()
                var quoted = [strip(trimmed, ">")]
                while let next = lines.first?.trimmingCharacters(in: .whitespaces), next.hasPrefix(">") {
                    quoted.append(strip(next, ">"))
                    lines = lines.dropFirst()
                }
                blocks.append(.quote(quoted))
                continue
            }

            if isTableRow(trimmed), let next = lines.first?.trimmingCharacters(in: .whitespaces),
               isTableDivider(next) {
                flushParagraph()
                lines = lines.dropFirst()
                var rows: [[String]] = []
                while let candidate = lines.first?.trimmingCharacters(in: .whitespaces),
                      isTableRow(candidate) {
                    rows.append(cells(candidate))
                    lines = lines.dropFirst()
                }
                blocks.append(.table(header: cells(trimmed), rows: rows))
                continue
            }

            if let first = listItem(trimmed) {
                flushParagraph()
                let ordered = first.ordered
                var items = [first.item]
                while let candidate = lines.first?.trimmingCharacters(in: .whitespaces),
                      let next = listItem(candidate), next.ordered == ordered {
                    items.append(next.item)
                    lines = lines.dropFirst()
                }
                blocks.append(.list(items: items, ordered: ordered))
                continue
            }

            paragraph.append(trimmed)
        }
        flushParagraph()
        return blocks
    }

    /// Renders one span of inline markdown, falling back to the raw text if it will not parse.
    ///
    /// Partial emphasis is normal mid-stream, so a failure here is expected rather than a fault.
    static func inline(_ text: String, base: Font) -> AttributedString {
        var out: AttributedString
        if let parsed = try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) {
            out = parsed
        } else {
            out = AttributedString(text)
        }
        out.font = base
        for run in out.runs where run.inlinePresentationIntent?.contains(.code) == true {
            out[run.range].font = Theme.Text.code
            out[run.range].backgroundColor = Theme.Colors.sunk
        }
        return out
    }

    private static func fenceLanguage(_ line: String) -> String? {
        guard line.hasPrefix("```") else { return nil }
        return String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
    }

    private static func isRule(_ line: String) -> Bool {
        let stripped = line.replacingOccurrences(of: " ", with: "")
        return stripped.count >= 3
            && (stripped.allSatisfy { $0 == "-" } || stripped.allSatisfy { $0 == "*" })
    }

    /// h1 is not supported, so `#` renders at h2. Dropping the line would lose the text.
    private static func heading(_ line: String) -> (Int, String)? {
        guard line.hasPrefix("#") else { return nil }
        let hashes = line.prefix { $0 == "#" }.count
        guard hashes <= 6, line.dropFirst(hashes).hasPrefix(" ") else { return nil }
        let body = String(line.dropFirst(hashes)).trimmingCharacters(in: .whitespaces)
        return (hashes <= 2 ? 2 : 3, body)
    }

    private static func listItem(_ line: String) -> (item: Item, ordered: Bool)? {
        for bullet in ["- ", "* ", "+ "] where line.hasPrefix(bullet) {
            let body = String(line.dropFirst(bullet.count))
            if body.hasPrefix("[ ] ") {
                return (Item(text: String(body.dropFirst(4)), checked: false), false)
            }
            if body.lowercased().hasPrefix("[x] ") {
                return (Item(text: String(body.dropFirst(4)), checked: true), false)
            }
            return (Item(text: body, checked: nil), false)
        }
        let digits = line.prefix { $0.isNumber }
        if !digits.isEmpty {
            let rest = line.dropFirst(digits.count)
            if rest.hasPrefix(". ") {
                return (Item(text: String(rest.dropFirst(2)), checked: nil), true)
            }
        }
        return nil
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
