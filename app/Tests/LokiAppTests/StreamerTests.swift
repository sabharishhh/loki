import Testing
@testable import LokiApp

/// The release rule, which is arithmetic and can be checked exhaustively.
///
/// Kept separate from the timed tests below on purpose. The drain runs at 60Hz in real time, so
/// exercising every backlog size end to end would take minutes; the rule is the part where a
/// mistake hides, and it costs nothing to check all of it.
struct StreamerReleaseTests {
    @Test("A release never asks for more than is waiting")
    func neverOverruns() {
        // `prefix` tolerates being asked for more than a collection holds and `removeFirst(_:)`
        // traps, so this is the exact invariant that crashed the app on almost every turn.
        for waiting in 1...20_000 {
            let take = Streamer.release(from: waiting)
            #expect(take >= 1)
            #expect(take <= waiting)
        }
    }

    @Test("Draining terminates and releases exactly what was waiting", arguments: [
        1, 2, 3, 4, 5, 6, 7, 11, 12, 13, 26, 97, 400, 3000, 20_000,
    ])
    func drainsExactly(waiting: Int) {
        var left = waiting
        var released = 0
        var frames = 0
        while left > 0 {
            let take = Streamer.release(from: left)
            left -= take
            released += take
            frames += 1
            #expect(frames <= waiting, "the loop is not making progress")
        }
        #expect(released == waiting)
    }

    @Test("A backlog at or above the floor releases at least the floor")
    func floorHolds() {
        for waiting in 3...500 {
            #expect(Streamer.release(from: waiting) >= 3)
        }
    }

    @Test("A large burst decays rather than emptying at a fixed rate")
    func largeBurstDecays() {
        // The proportional rule is what keeps a whole answer arriving at once from dumping. If it
        // were replaced by the floor alone, 3000 characters would take 1000 frames.
        #expect(Streamer.release(from: 3000) == 500)
        #expect(Streamer.release(from: 400) == 66)
    }
}

/// The parts that need the loop to actually run.
@MainActor
struct StreamerDrainTests {
    /// Waits for the drain to report that everything is on screen.
    ///
    /// The drain lives in a `Task` the class owns, so there is nothing to await directly. `onIdle`
    /// is the signal the conversation itself uses to decide a turn is over, which makes it the
    /// right thing to test against rather than a sleep of a guessed length.
    private func drained(
        _ build: (Streamer) -> Void
    ) async -> String {
        var shown = ""
        let streamer = Streamer { shown += $0 }
        await confirmation("the backlog empties") { done in
            streamer.onIdle = { done() }
            build(streamer)
            while !Task.isCancelled, streamer.isDraining {
                try? await Task.sleep(for: .milliseconds(4))
            }
        }
        return shown
    }

    @Test("Text arrives whole and in order")
    func textIsIntact() async {
        let answer = "Nice to be back. Glad you are doing well."
        let shown = await drained { streamer in
            for token in answer.split(separator: " ", omittingEmptySubsequences: false) {
                streamer.accept(token + " ")
            }
            streamer.finish()
        }
        #expect(shown.trimmingCharacters(in: .whitespaces) == answer)
    }

    @Test("One character on its own is shown, not trapped on")
    func oneCharacter() async {
        // The crash case. A reply shorter than the release floor was the common shape, because
        // every answer ends with one.
        let shown = await drained { streamer in
            streamer.accept("4")
            streamer.finish()
        }
        #expect(shown == "4")
    }

    @Test("A turn that produced nothing still reports itself finished")
    func silentTurn() async {
        // A blocked or failed turn streams no tokens at all. If the drain only reported idle after
        // showing something, the composer would stay in its running state with nothing coming.
        let shown = await drained { streamer in
            streamer.finish()
        }
        #expect(shown.isEmpty)
    }

    @Test("A grapheme is never cut in half")
    func graphemesSurvive() async {
        // Characters, not bytes. A family emoji is one Character made of several scalars, and a
        // flag is two; splitting either renders a fragment for a frame.
        let answer = "👩‍👩‍👧‍👦🇮🇳é🇮🇳👩‍👩‍👧‍👦"
        let shown = await drained { streamer in
            streamer.accept(answer)
            streamer.finish()
        }
        #expect(shown == answer)
        #expect(shown.count == answer.count)
    }

    @Test("An interrupt shows everything already received, at once")
    func flushShowsTheRest() async {
        var shown = ""
        let streamer = Streamer { shown += $0 }
        streamer.accept("a long answer that was cut off partway through")
        streamer.flush()
        // No waiting. The cut mark goes under the text, so the text cannot still be arriving.
        #expect(shown == "a long answer that was cut off partway through")
    }

    @Test("Starting a turn drops what the last one left behind")
    func resetDropsPending() async {
        var shown = ""
        var idled = false
        let streamer = Streamer { shown += $0 }
        streamer.onIdle = { idled = true }
        streamer.accept("stale")
        streamer.reset()
        #expect(shown.isEmpty)
        // Reset is not the end of a turn, and reporting one here would end the turn that is
        // starting.
        #expect(idled == false)
    }
}
