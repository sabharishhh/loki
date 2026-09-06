import Foundation

/// Reading the `[1]` markers a model writes into an answer (§12.7).
///
/// **The markers are instructions to the interface, not text for the reader.** `Found::brief`
/// numbers the sources it hands the model and asks it to cite them by number, so a bare `[1]` left
/// on screen is the mechanism showing through. Every valid marker comes out of the prose and comes
/// back as a chip that names the site it points at.
///
/// **Only markers that point at something are touched.** A model writing `[1]` when one source was
/// given is citing; an answer about arrays writing `a[0]` or a reference to `[2]` when two sources
/// exist and the second is meant is not always separable, so the rule is narrow and stated: a
/// marker counts when it is a bracketed number with no adjacent word character and it indexes a
/// source that exists. Anything else stays exactly as the model wrote it.
enum Citations {
    /// A paragraph with its citation markers taken out, and the sources they named.
    struct Split: Equatable {
        var text: String
        /// In the order the model cited them, each appearing once.
        var cited: [Int]
    }

    /// Splits one block of prose from the markers inside it.
    ///
    /// `available` is how many sources the turn has, so a marker pointing past the end is left as
    /// written rather than silently dropped: an answer citing `[9]` with three sources is wrong in
    /// a way the reader should be able to see.
    static func split(_ text: String, available: Int) -> Split {
        guard available > 0, text.contains("[") else {
            return Split(text: text, cited: [])
        }

        var body = ""
        var cited: [Int] = []
        var rest = Substring(text)

        while let open = rest.firstIndex(of: "[") {
            guard let close = rest[open...].firstIndex(of: "]") else { break }
            let inside = rest[rest.index(after: open)..<close]
            let before = rest[..<open]
            let after = rest[rest.index(after: close)...]

            let number = Int(inside)
            let isCitation =
                number.map { $0 >= 1 && $0 <= available } == true
                && !inside.isEmpty
                // `a[1]` is an index into an array, not a citation. Nothing may touch the bracket.
                && before.last.map { !$0.isLetter && !$0.isNumber && $0 != "_" } != false
                // `[1](url)` is a markdown link whose text happens to be a number.
                && after.first != "("

            body += before
            if isCitation, let number {
                if !cited.contains(number) { cited.append(number) }
            } else {
                body += rest[open...close]
            }
            rest = after
        }
        body += rest

        guard !cited.isEmpty else { return Split(text: text, cited: []) }
        return Split(text: tidied(body), cited: cited)
    }

    /// Closes the gaps a removed marker leaves behind.
    ///
    /// "Hormuz [1]." has to become "Hormuz." rather than "Hormuz ." A space before punctuation is
    /// the giveaway that something was lifted out of the sentence.
    private static func tidied(_ text: String) -> String {
        // Whitespace first, then punctuation. Two markers in a row leave two spaces, and fixing
        // the punctuation before collapsing them only ever removes one of the pair.
        var out = text
        while out.contains("  ") {
            out = out.replacingOccurrences(of: "  ", with: " ")
        }
        for mark in [".", ",", ";", ":", "!", "?"] {
            out = out.replacingOccurrences(of: " \(mark)", with: mark)
        }
        return out.trimmingCharacters(in: .whitespaces)
    }
}
