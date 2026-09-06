import Testing

@testable import LokiApp

@Suite("Reading citation markers")
struct CitationsTests {
    @Test("A cited sentence keeps its prose and loses the marker")
    func markerComesOut() {
        let split = Citations.split("Rust 1.96 is the latest release [1].", available: 2)
        #expect(split.text == "Rust 1.96 is the latest release.")
        #expect(split.cited == [1])
    }

    @Test("Several sources on one claim come back in the order they were cited")
    func severalSources() {
        let split = Citations.split("The river crossed its flood mark [2] [1].", available: 3)
        #expect(split.cited == [2, 1])
        #expect(split.text == "The river crossed its flood mark.")
    }

    @Test("The same source cited twice is one chip")
    func deduped() {
        let split = Citations.split("First [1]. Second [1].", available: 1)
        #expect(split.cited == [1])
    }

    /// An index into an array is not a citation, and an answer about code is full of them.
    @Test("Array subscripts are left alone")
    func subscriptsSurvive() {
        let split = Citations.split("Use items[0] and items[1] together.", available: 3)
        #expect(split.cited.isEmpty)
        #expect(split.text == "Use items[0] and items[1] together.")
    }

    /// **A marker pointing past the end stays visible.** An answer citing a source that was never
    /// given is wrong, and quietly deleting the evidence of that is the worst of the options.
    @Test("A marker with no source behind it is left as written")
    func danglingMarkerStays() {
        let split = Citations.split("As reported [9].", available: 2)
        #expect(split.cited.isEmpty)
        #expect(split.text == "As reported [9].")
    }

    @Test("A markdown link whose text is a number is still a link")
    func numberedLinkSurvives() {
        let split = Citations.split("See [1](https://example.com) for more.", available: 2)
        #expect(split.cited.isEmpty)
        #expect(split.text.contains("[1](https://example.com)"))
    }

    @Test("An answer with no markers is returned untouched")
    func nothingToDo() {
        let plain = "No sources were needed here."
        #expect(Citations.split(plain, available: 3).text == plain)
        #expect(Citations.split("Cited [1] but nothing was found.", available: 0).text
            == "Cited [1] but nothing was found.")
    }
}
