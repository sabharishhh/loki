import Testing

@testable import LokiApp

/// The trace's own bookkeeping, which is where the ladder stops being a list of attempts and
/// becomes a list of pages.
struct SearchTraceTests {
    /// Reported as it happened: two rungs tried on one page, then a different page read.
    ///
    /// Before this, each attempt got a row of its own, and the rows named no page at all, so three
    /// unreadable pages read as six copies of "Rung 1 said blocked".
    @Test func twoRungsOnOnePageShareOneRow() {
        var steps: [SearchStep] = []
        steps.advance(with: SearchStep(kind: .searching(query: "ml jobs")))
        steps.advance(with: SearchStep(kind: .rung(host: "indeed.com", number: 1, verdict: "blocked")))
        steps.advance(with: SearchStep(kind: .rung(host: "indeed.com", number: 2, verdict: "blocked")))
        steps.advance(with: SearchStep(kind: .reading(host: "mljobs.io")))

        #expect(steps.count == 3)
        #expect(steps[1].kind == .rung(host: "indeed.com", number: 2, verdict: "blocked"))
    }

    /// Two pages that both failed are two facts, not one. Collapsing by position rather than by
    /// host would have hidden the second.
    @Test func twoPagesThatBothFailedKeepTheirOwnRows() {
        var steps: [SearchStep] = []
        steps.advance(with: SearchStep(kind: .rung(host: "indeed.com", number: 1, verdict: "blocked")))
        steps.advance(with: SearchStep(kind: .rung(host: "naukri.com", number: 1, verdict: "blocked")))

        #expect(steps.count == 2)
    }

    /// A page tried again after another page was tried in between is a new row, because the row
    /// above it is no longer about it.
    @Test func aPageRetriedLaterDoesNotReachBackwards() {
        var steps: [SearchStep] = []
        steps.advance(with: SearchStep(kind: .rung(host: "indeed.com", number: 1, verdict: "blocked")))
        steps.advance(with: SearchStep(kind: .rung(host: "naukri.com", number: 1, verdict: "blocked")))
        steps.advance(with: SearchStep(kind: .rung(host: "indeed.com", number: 2, verdict: "blocked")))

        #expect(steps.count == 3)
    }

    /// Everything before the newest step is finished, which is what stops two rows shimmering.
    @Test func onlyTheNewestStepIsStillRunning() {
        var steps: [SearchStep] = []
        steps.advance(with: SearchStep(kind: .searching(query: "ml jobs")))
        steps.advance(with: SearchStep(kind: .reading(host: "mljobs.io")))

        #expect(steps[0].done)
        #expect(!steps[1].done)
    }

    /// The core's verdict vocabulary is not English. A reader should not have to know what
    /// `js_required` means to read their own trace.
    @Test func averdictIsSaidInWordsAReaderHasMet() {
        let step = SearchStep.Kind.rung(host: "indeed.com", number: 2, verdict: "js_required")
        #expect(step.sentence == "indeed.com: needs JavaScript at rung 2")
    }
}
