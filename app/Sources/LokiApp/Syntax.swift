import SwiftUI

/// Syntax colouring inside a fenced code block.
///
/// No new hues. The design system allows four accents and says colour means machine state, so a
/// fifth colour for code would break the rule that makes the four legible. Code reuses the same
/// four: keywords in `reading`, strings in `released`, numbers in `holding`, comments in
/// `--faint`. `needsYou` is deliberately unused, so the error colour never appears in code by
/// accident.
enum Syntax {
    /// Colours `code` for `language`. An unknown language still gets strings, numbers and
    /// comments, which is most of the value.
    static func highlight(_ code: String, language: String?) -> AttributedString {
        let dialect = Dialect(language)
        var out = AttributedString(code)
        out.font = Theme.Text.code
        out.foregroundColor = Theme.Colors.primary

        for token in scan(code, dialect) {
            guard let range = Range(token.range, in: out) else { continue }
            out[range].foregroundColor = token.kind.color
        }
        return out
    }

    private enum Kind {
        case comment, string, number, keyword

        /// Four values from the palette, and four that are actually different from each other.
        ///
        /// These were written as state colours, which worked while there were four states with
        /// four hues. Dropping `released` left `thinking` and `reading` both on the accent, so a
        /// keyword and a number became the same yellow and a string and a comment the same grey:
        /// four token kinds rendering as two. A code well is not a state readout, so it takes
        /// ordinary palette values rather than borrowing meanings that have since merged (B-69).
        var color: Color {
            switch self {
            case .comment: Theme.Colors.tertiary
            case .string: Theme.Colors.secondary
            case .number: Theme.Colors.yellowHover
            case .keyword: Theme.Colors.yellow
            }
        }
    }

    private struct Token {
        let range: Range<String.Index>
        let kind: Kind
    }

    /// What a language's comments, strings and keywords look like.
    private struct Dialect {
        let lineComments: [String]
        let blockComment: (open: String, close: String)?
        let quotes: Set<Character>
        let keywords: Set<String>

        init(_ language: String?) {
            let name = (language ?? "").lowercased()
            switch name {
            case "bash", "sh", "zsh", "shell", "console", "terminal":
                lineComments = ["#"]
                blockComment = nil
                quotes = ["\"", "'"]
                keywords = Self.shell
            case "python", "py":
                lineComments = ["#"]
                blockComment = nil
                quotes = ["\"", "'"]
                keywords = Self.python
            case "swift":
                lineComments = ["//"]
                blockComment = ("/*", "*/")
                quotes = ["\""]
                keywords = Self.swift
            case "rust", "rs":
                lineComments = ["//"]
                blockComment = ("/*", "*/")
                quotes = ["\""]
                keywords = Self.rust
            case "javascript", "js", "typescript", "ts", "tsx", "jsx":
                lineComments = ["//"]
                blockComment = ("/*", "*/")
                quotes = ["\"", "'", "`"]
                keywords = Self.javascript
            case "json":
                lineComments = []
                blockComment = nil
                quotes = ["\""]
                keywords = ["true", "false", "null"]
            case "yaml", "yml", "toml":
                lineComments = ["#"]
                blockComment = nil
                quotes = ["\"", "'"]
                keywords = ["true", "false", "null", "yes", "no"]
            case "sql":
                lineComments = ["--"]
                blockComment = ("/*", "*/")
                quotes = ["'", "\""]
                keywords = Self.sql
            case "go", "java", "c", "cpp", "c++", "csharp", "cs", "kotlin":
                lineComments = ["//"]
                blockComment = ("/*", "*/")
                quotes = ["\"", "'"]
                keywords = Self.curly
            default:
                // Unknown language. Strings, numbers and both common comment markers still read.
                lineComments = ["#", "//"]
                blockComment = ("/*", "*/")
                quotes = ["\"", "'"]
                keywords = []
            }
        }

        static let shell: Set<String> = [
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
            "function", "return", "export", "local", "readonly", "in", "echo", "cd", "set",
        ]
        static let python: Set<String> = [
            "def", "class", "return", "if", "elif", "else", "for", "while", "in", "not", "and",
            "or", "import", "from", "as", "with", "try", "except", "finally", "raise", "lambda",
            "None", "True", "False", "pass", "break", "continue", "yield", "async", "await",
        ]
        static let swift: Set<String> = [
            "func", "var", "let", "class", "struct", "enum", "protocol", "extension", "if", "else",
            "guard", "for", "while", "in", "return", "switch", "case", "default", "break",
            "continue", "import", "init", "self", "nil", "true", "false", "throws", "try", "catch",
            "async", "await", "actor", "private", "public", "internal", "static", "some", "any",
            "where", "as", "is", "defer", "typealias", "mutating", "override", "final", "lazy",
        ]
        static let rust: Set<String> = [
            "fn", "let", "mut", "const", "static", "struct", "enum", "trait", "impl", "for",
            "while", "loop", "if", "else", "match", "return", "break", "continue", "use", "mod",
            "pub", "crate", "self", "super", "where", "as", "dyn", "ref", "move", "async", "await",
            "unsafe", "true", "false", "Some", "None", "Ok", "Err", "type",
        ]
        static let javascript: Set<String> = [
            "function", "const", "let", "var", "class", "extends", "return", "if", "else", "for",
            "while", "of", "in", "new", "this", "null", "undefined", "true", "false", "import",
            "from", "export", "default", "async", "await", "try", "catch", "finally", "throw",
            "typeof", "instanceof", "switch", "case", "break", "continue", "interface", "type",
        ]
        static let sql: Set<String> = [
            "select", "from", "where", "insert", "into", "values", "update", "set", "delete",
            "create", "table", "index", "drop", "alter", "join", "left", "right", "inner", "outer",
            "on", "group", "by", "order", "having", "limit", "offset", "and", "or", "not", "null",
            "primary", "key", "foreign", "references", "as", "distinct", "union", "with",
        ]
        static let curly: Set<String> = [
            "func", "var", "const", "class", "struct", "interface", "return", "if", "else", "for",
            "while", "range", "switch", "case", "default", "break", "continue", "import",
            "package", "type", "nil", "null", "true", "false", "new", "public", "private",
            "static", "void", "int", "string", "bool", "try", "catch", "throw", "finally",
        ]
    }

    /// One left-to-right pass. Comments and strings win over keywords, because a keyword inside a
    /// string is not a keyword.
    private static func scan(_ code: String, _ dialect: Dialect) -> [Token] {
        var tokens: [Token] = []
        var i = code.startIndex

        while i < code.endIndex {
            let rest = code[i...]

            if let block = dialect.blockComment, rest.hasPrefix(block.open) {
                let end = rest.range(of: block.close).map(\.upperBound) ?? code.endIndex
                tokens.append(Token(range: i..<end, kind: .comment))
                i = end
                continue
            }

            if let marker = dialect.lineComments.first(where: { rest.hasPrefix($0) }) {
                _ = marker
                let end = rest.firstIndex(of: "\n") ?? code.endIndex
                tokens.append(Token(range: i..<end, kind: .comment))
                i = end
                continue
            }

            let ch = code[i]
            if dialect.quotes.contains(ch) {
                var j = code.index(after: i)
                while j < code.endIndex {
                    if code[j] == "\\" {
                        j = code.index(j, offsetBy: 2, limitedBy: code.endIndex) ?? code.endIndex
                        continue
                    }
                    if code[j] == ch { j = code.index(after: j); break }
                    // An unterminated string stops at the line end rather than eating the file.
                    if code[j] == "\n" { break }
                    j = code.index(after: j)
                }
                tokens.append(Token(range: i..<j, kind: .string))
                i = j
                continue
            }

            if ch.isNumber, !isWordCharacter(previous(of: i, in: code)) {
                var j = i
                while j < code.endIndex, code[j].isNumber || code[j] == "." || code[j] == "_" {
                    j = code.index(after: j)
                }
                tokens.append(Token(range: i..<j, kind: .number))
                i = j
                continue
            }

            if isWordStart(ch), !isWordCharacter(previous(of: i, in: code)) {
                var j = i
                while j < code.endIndex, isWordCharacter(code[j]) {
                    j = code.index(after: j)
                }
                if dialect.keywords.contains(String(code[i..<j])) {
                    tokens.append(Token(range: i..<j, kind: .keyword))
                }
                i = j
                continue
            }

            i = code.index(after: i)
        }
        return tokens
    }

    private static func previous(of index: String.Index, in code: String) -> Character? {
        guard index > code.startIndex else { return nil }
        return code[code.index(before: index)]
    }

    private static func isWordStart(_ ch: Character) -> Bool {
        ch.isLetter || ch == "_"
    }

    private static func isWordCharacter(_ ch: Character?) -> Bool {
        guard let ch else { return false }
        return ch.isLetter || ch.isNumber || ch == "_"
    }
}
