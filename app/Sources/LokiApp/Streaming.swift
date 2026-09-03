import Foundation

/// Smooths the model's token stream into a steady flow.
///
/// **Why this exists.** A provider does not send tokens evenly. They arrive in bursts, with gaps,
/// and rendering each one the instant it lands makes the text jump: a paragraph appears whole,
/// then nothing for 300ms, then three more. Every one of those jumps relays out the block tree
/// below it, which is where the stray line breaks came from.
///
/// So the tokens land here and the view reads from here on a fixed cadence. The provider's timing
/// stops being the interface's timing.
///
/// **The drain is proportional, not constant.** Each tick releases a fraction of what is waiting,
/// so the backlog decays rather than emptying at a fixed characters-per-second. One rule covers
/// both ends of the range with no mode to pick, because the time to clear grows with the log of
/// the backlog rather than with the backlog. Measured against this arithmetic:
///
/// ```text
///      8 chars   128 ms      a one-line answer, present almost at once
///     26 chars   256 ms
///    400 chars   512 ms
///   3000 chars   688 ms      a whole answer arriving in one burst still reveals, not dumps
/// ```
///
/// While the model is still talking the backlog stays small: at a realistic 40 characters every
/// 96 ms it settles around 50 characters, roughly 320 ms behind, which is close enough that the
/// text reads as arriving live.
///
/// A constant rate cannot do both. Fast enough for a long answer is a stutter on a short one, and
/// comfortable for a short one leaves a long one seconds behind.
@MainActor
final class Streamer {
    /// One frame at 60Hz. Faster buys nothing a display can show; slower reads as stutter.
    private static let tick = Duration.milliseconds(16)

    /// Fraction of the backlog released per tick, as a divisor.
    ///
    /// Six is the one number worth tuning here. Lower reads as frantic on a short answer; higher
    /// leaves a long one visibly trailing the model.
    private static let share = 6

    private var pending: [Character] = []
    private var draining: Task<Void, Never>?
    private var closed = false

    /// Called with each batch of characters to show, on the main actor.
    private let emit: (String) -> Void

    init(emit: @escaping (String) -> Void) {
        self.emit = emit
    }

    /// Takes a token from the provider. Nothing reaches the view yet.
    func accept(_ token: String) {
        pending.append(contentsOf: token)
        start()
    }

    /// The model has stopped. Keeps flowing until the backlog is empty, then stops.
    ///
    /// Not a flush: cutting to the end at the last token would make every answer end with a jump,
    /// which is the artefact this class exists to remove.
    func finish() {
        closed = true
        start()
    }

    /// Shows everything at once and stops.
    ///
    /// For an interrupt. §18.3 puts the cut mark where the text stopped, and a mark that appears
    /// above text still trickling in would be pointing at the wrong place.
    func flush() {
        draining?.cancel()
        draining = nil
        closed = true
        if !pending.isEmpty {
            emit(String(pending))
            pending.removeAll()
        }
    }

    /// Drops everything without showing it. For starting a new turn.
    func reset() {
        draining?.cancel()
        draining = nil
        pending.removeAll()
        closed = false
    }

    private func start() {
        guard draining == nil else { return }
        draining = Task { [weak self] in
            while true {
                guard let self, self.step() else { return }
                try? await Task.sleep(for: Self.tick)
            }
        }
    }

    /// One frame's worth. Returns whether there is more to do.
    private func step() -> Bool {
        if pending.isEmpty {
            // Keep the loop alive while the model is still talking, or the next token pays for a
            // fresh task and the gap shows up as a stall.
            if closed {
                draining = nil
                return false
            }
            return true
        }

        // Characters rather than bytes, so a grapheme is never cut in half. An emoji or a combined
        // mark rendered as a fragment for one frame is exactly the flicker this removes.
        let take = max(1, pending.count / Self.share)
        emit(String(pending.prefix(take)))
        pending.removeFirst(take)
        return true
    }
}
