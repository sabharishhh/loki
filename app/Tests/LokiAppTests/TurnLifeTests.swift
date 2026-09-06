import Testing

@testable import LokiApp

@Suite("Which trace is counting")
struct TurnLifeTests {
    @Test("An open scope counts")
    func openScopeCounts() {
        var life = TurnLife()
        life.begin()
        life.opened(0)
        #expect(life.isLive(scope: 0, closed: false))
    }

    @Test("A closed scope stops when the answer has finished painting")
    func closedScopeStops() {
        var life = TurnLife()
        life.begin()
        life.opened(0)
        #expect(life.isLive(scope: 0, closed: true), "still painting")
        life.end()
        #expect(!life.isLive(scope: 0, closed: true))
    }

    /// B-75. The next turn sets `working` before the core has opened a scope, and in that gap the
    /// finished turn's trace must not come back to life: it would recompute its age from a start a
    /// whole turn ago. A 1.1s greeting displayed "Thought for 61s", which was the gap to the next
    /// question.
    @Test("A finished turn's trace does not restart when the next turn begins")
    func aFinishedTraceStaysFinished() {
        var life = TurnLife()
        life.begin()
        life.opened(0)
        life.end()

        life.begin()
        #expect(
            !life.isLive(scope: 0, closed: true),
            "the previous turn's trace is live again before this turn's scope opens"
        )

        life.opened(1)
        #expect(!life.isLive(scope: 0, closed: true))
        #expect(life.isLive(scope: 1, closed: true), "this turn's trace is the one counting")
    }

    @Test("Beginning twice in one turn keeps the first clock")
    func beginIsIdempotent() {
        var life = TurnLife()
        life.begin()
        let first = life.began
        life.begin()
        #expect(life.began == first, "the wait started when the reader pressed send")
    }
}
