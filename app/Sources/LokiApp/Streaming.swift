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
/// **The drain is proportional, with a floor.** Each tick releases a fraction of what is waiting,
/// so a burst decays rather than emptying at a fixed characters-per-second, and the floor stops
/// the opening and the tail of that decay from crawling. Measured against this arithmetic:
///
/// ```text
///      8 chars    48 ms      a one-line answer, present almost at once
///     26 chars   144 ms
///    400 chars   384 ms
///   3000 chars   560 ms      a whole answer arriving in one burst still reveals, not dumps
/// ```
///
/// **The floor is what makes the first words readable.** Without it the release is `pending / 6`
/// rounded down, which is zero for any backlog under six and clamps to one character a frame. A
/// steady stream outruns that quickly, because the backlog grows until the proportion catches up,
/// but the opening of an answer has no backlog to grow: the first dozen characters of every reply
/// went out at 62 a second, one letter at a time, which is what made a short answer look like it
/// had stalled after its first character. The floor holds the release at 187 a second, faster than
/// any provider streams, so the start of an answer is paced by the model and not by this class
/// (B-60).
///
/// A constant rate cannot do both ends. Fast enough for a long answer is a stutter on a short one,
/// and comfortable for a short one leaves a long one seconds behind.
@MainActor
final class Streamer {
    /// One frame at 60Hz. Faster buys nothing a display can show; slower reads as stutter.
    private static let tick = Duration.milliseconds(16)

    /// Fraction of the backlog released per tick, as a divisor.
    ///
    /// Six is the one number worth tuning here. Lower reads as frantic on a short answer; higher
    /// leaves a long one visibly trailing the model.
    private nonisolated static let share = 6

    /// The least a tick may release. Bounds the lag at `floor` characters per frame, which is
    /// 187 a second: faster than any provider streams, so the backlog is what decays and never
    /// the display.
    private nonisolated static let floor = 3

    private var pending: [Character] = []
    private var draining: Task<Void, Never>?
    private var closed = false

    /// Called with each batch of characters to show, on the main actor.
    private let emit: (String) -> Void

    /// Called once the backlog empties after `finish`, so a caller can tell "fully received" from
    /// "fully on screen". The thinking trace stops its clock on the second, because that is the
    /// one the reader waited for.
    var onIdle: (() -> Void)?

    init(emit: @escaping (String) -> Void) {
        self.emit = emit
    }

    /// How many characters one frame releases from a backlog of `waiting`.
    ///
    /// Clamped to what is there. `prefix` tolerates being asked for more than a collection holds
    /// and `removeFirst(_:)` traps, so the floor read as safe beside one and crashed against the
    /// other on any backlog shorter than it, which is most of them (B-66).
    ///
    /// - Precondition: `waiting` is at least 1. Returns at least 1 and never more than `waiting`.
    nonisolated static func release(from waiting: Int) -> Int {
        min(waiting, max(floor, waiting / share))
    }

    /// Whether the loop is still running. For tests, which have nothing else to wait on.
    var isDraining: Bool { draining != nil }

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
        onIdle?()
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
            // Cancellation has to be read here, not left to the sleep. `Task.sleep` on a
            // cancelled task returns at once, so a loop that only checks `step` spins on the main
            // actor at full speed and never exits: `reset` clears `closed`, which is the one
            // condition that ends it. One zombie per interrupted turn, each holding the actor the
            // interface draws on.
            while !Task.isCancelled {
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
                onIdle?()
                return false
            }
            return true
        }

        // Characters rather than bytes, so a grapheme is never cut in half. An emoji or a combined
        // mark rendered as a fragment for one frame is exactly the flicker this removes.
        let take = Self.release(from: pending.count)
        emit(String(pending.prefix(take)))
        pending.removeFirst(take)
        return true
    }
}
