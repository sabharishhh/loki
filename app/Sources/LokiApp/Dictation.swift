// AVAudioConverterInputBlock is declared @Sendable, but `convert` invokes it synchronously on the
// calling thread and returns before it can be called again. Nothing it captures escapes or races.
// AVFAudio predates strict concurrency and carries no annotation saying so. Drop this once the
// framework is audited.
@preconcurrency import AVFoundation
import Foundation
import Speech

/// On-device dictation.
///
/// Audio never leaves the Mac and never crosses the bridge. The Rust core receives text only,
/// which keeps platform media code out of it.
///
/// Capture goes through `AVCaptureSession` rather than `AVAudioEngine.installTap`, because the
/// tap does not fire with a Bluetooth input device.
@MainActor
@Observable
final class Dictation {
    enum Status: Equatable {
        case idle
        case denied
        case preparing
        case listening
        /// Flushing the analyzer. Not listening any more, and not ready to start again either.
        case stopping
        case unavailable(String)
    }

    private(set) var status: Status = .idle
    /// Text so far this utterance, including the volatile tail.
    private(set) var transcript = ""
    /// Recent input loudness, 0 to 1, oldest first. Drives the waveform.
    private(set) var levels: [Float] = []

    /// Fires the moment speech is detected, before transcription finishes.
    ///
    /// Talking over a running task is an interrupt, and the visible stop has a 150ms budget, so
    /// the interrupt cannot wait for words.
    var onSpeechStart: (() -> Void)?

    private var session: AVCaptureSession?
    private var analyzer: SpeechAnalyzer?
    private var inputContinuation: AsyncStream<AnalyzerInput>.Continuation?
    private var tasks: [Task<Void, Never>] = []
    /// The provisional text, from the fast transcriber. What the field shows while you speak.
    private var heard = Transcript()
    /// The finished text, from the accurate transcriber. What the draft ends up holding.
    private var spoken = Transcript()

    var isListening: Bool { status == .listening }

    /// Whether a recording is underway, including the moment before the first audio arrives.
    ///
    /// The control reads this rather than `isListening`. Starting takes a beat, and a second
    /// click in that window was being swallowed, which left the microphone on with no way off.
    var isRecording: Bool { status == .preparing || status == .listening }

    /// The most recent levels, for a compact meter.
    func recentLevels(_ count: Int) -> [Float] {
        Array(levels.suffix(count))
    }

    /// Begins an utterance. Idempotent.
    ///
    /// A previous failure is not a reason to refuse the next attempt, so `.unavailable` and
    /// `.denied` both restart.
    func start() async {
        guard canStart else { return }
        status = .preparing
        transcript = ""
        heard = Transcript()
        spoken = Transcript()
        levels = []

        guard await AVCaptureDevice.requestAccess(for: .audio) else {
            status = .denied
            return
        }

        do {
            try await begin()
            status = .listening
        } catch {
            await teardown()
            status = .unavailable(String(describing: error))
        }
    }

    /// Ends the utterance and returns the finished text.
    ///
    /// Order matters. `finalizeAndFinishThroughEndOfInput` waits for the input sequence to end, so
    /// the microphone and the input stream have to be closed before it is called. Finalizing
    /// first waits forever on a stream nothing will ever close, and a stop that never returns
    /// leaves the mic stuck on and the next utterance appended to this one.
    @discardableResult
    func stop() async -> String {
        guard status == .listening || status == .preparing else { return "" }
        // Held through the flush. A click arriving mid-teardown would otherwise start a session
        // whose analyzer this teardown then releases.
        status = .stopping

        endInput()
        await finalize()
        releaseAnalyzer()

        // The accurate transcriber is the one whose words are kept. It falls behind while you
        // speak and catches up on the flush, so it is only read here. If it produced nothing at
        // all, the provisional text is better than losing the utterance.
        let accurate = spoken.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let provisional = heard.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let text = accurate.isEmpty ? provisional : accurate

        transcript = text
        levels = []
        status = .idle
        return text
    }

    private var canStart: Bool {
        switch status {
        case .idle, .denied, .unavailable: true
        case .preparing, .listening, .stopping: false
        }
    }

    /// Closes the microphone and the input stream. Must happen before finalizing.
    private func endInput() {
        if let session {
            // stopRunning blocks, and the session is not touched again.
            nonisolated(unsafe) let stopping = session
            Task.detached { stopping.stopRunning() }
        }
        session = nil
        pump = nil
        inputContinuation?.finish()
        inputContinuation = nil
    }

    /// Flushes the last words out of the analyzer.
    ///
    /// With the input already closed this returns in about a tenth of a second, measured. The
    /// bound is defence: a wedged analyzer must not be able to leave the microphone stuck on.
    /// Losing the tail of an utterance is recoverable; a stuck microphone is not.
    ///
    /// Cancelling the flush does not interrupt it, so the bound cancels a *waiter* instead and
    /// lets the flush finish on its own. Awaiting a throwing task's value is cancellation-aware,
    /// which is what makes that work.
    private func finalize() async {
        guard let analyzer else { return }
        let flush = Task.detached { try await analyzer.finalizeAndFinishThroughEndOfInput() }

        let waiter = Task { try await flush.value }
        let bound = Task {
            try? await Task.sleep(for: .seconds(2))
            waiter.cancel()
        }
        _ = try? await waiter.value
        bound.cancel()
    }

    private func releaseAnalyzer() {
        analyzer = nil
        tasks.forEach { $0.cancel() }
        tasks.removeAll()
    }

    private func begin() async throws {
        let locale = Locale.current
        // Two transcribers, because neither is good at both jobs and one analyzer will run both.
        //
        // `DictationTranscriber` is fast: first partial at 0.6s against 4.3s, which is the whole
        // reason the field can fill while you speak. It is also materially less accurate. On a
        // recording of "Hey Loki, how are you?" it returned "Halo, how are you?" and dropped the
        // name outright, where `SpeechTranscriber` got the sentence exactly.
        //
        // So the fast one supplies the grey provisional text and the accurate one supplies the
        // words that are kept. Grey already means "not committed yet", which is exactly the claim
        // the fast transcriber can support.
        let fast = DictationTranscriber(
            locale: locale,
            contentHints: [.shortForm],
            transcriptionOptions: [.punctuation],
            reportingOptions: [.volatileResults, .frequentFinalization],
            attributeOptions: []
        )
        let accurate = SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [.volatileResults],
            attributeOptions: []
        )
        let detector = SpeechDetector(detectionOptions: .init(sensitivityLevel: .medium),
                                      reportResults: true)
        let modules: [any SpeechModule] = [fast, accurate, detector]

        // The model is managed by the OS. This downloads it the first time only.
        if let request = try await AssetInventory.assetInstallationRequest(supporting: modules) {
            try await request.downloadAndInstall()
        }

        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: modules)
        else {
            throw DictationError.noCompatibleFormat
        }

        let (inputs, continuation) = AsyncStream<AnalyzerInput>.makeStream()
        inputContinuation = continuation

        let analyzer = SpeechAnalyzer(modules: modules)
        try await analyzer.prepareToAnalyze(in: format)
        try await analyzer.start(inputSequence: inputs)
        self.analyzer = analyzer

        tasks.append(Task { [weak self] in
            guard let results = self?.fastResults(fast) else { return }
            for await segment in results { self?.absorbHeard(segment) }
        })
        tasks.append(Task { [weak self] in
            guard let results = self?.accurateResults(accurate) else { return }
            for await segment in results { self?.absorbSpoken(segment) }
        })
        tasks.append(Task { [weak self] in
            guard let detections = self?.detectorResults(detector) else { return }
            for await detected in detections where detected {
                self?.onSpeechStart?()
            }
        })

        try startCapture(feeding: continuation, converting: format)
    }

    /// Bridges the fast transcriber's throwing sequence into plain segments.
    private func fastResults(_ transcriber: DictationTranscriber) -> AsyncStream<Segment> {
        AsyncStream { continuation in
            Task {
                do {
                    for try await result in transcriber.results {
                        continuation.yield(Segment(
                            start: result.range.start.seconds,
                            text: String(result.text.characters),
                            isFinal: result.isFinal
                        ))
                    }
                } catch {}
                continuation.finish()
            }
        }
    }

    /// Bridges the accurate transcriber's throwing sequence into plain segments.
    ///
    /// It has no `isFinal`, so finality is a comparison: a result is settled once the analyzer
    /// says it has finalized everything up to the end of that result's range.
    private func accurateResults(_ transcriber: SpeechTranscriber) -> AsyncStream<Segment> {
        AsyncStream { continuation in
            Task {
                do {
                    for try await result in transcriber.results {
                        continuation.yield(Segment(
                            start: result.range.start.seconds,
                            text: String(result.text.characters),
                            isFinal: result.resultsFinalizationTime >= result.range.end
                        ))
                    }
                } catch {}
                continuation.finish()
            }
        }
    }

    private func detectorResults(_ detector: SpeechDetector) -> AsyncStream<Bool> {
        AsyncStream { continuation in
            Task {
                do {
                    for try await result in detector.results {
                        continuation.yield(result.speechDetected)
                    }
                } catch {}
                continuation.finish()
            }
        }
    }

    private func absorbHeard(_ segment: Segment) {
        heard.absorb(segment)
        transcript = heard.text
    }

    private func absorbSpoken(_ segment: Segment) {
        spoken.absorb(segment)
    }

    private func startCapture(
        feeding continuation: AsyncStream<AnalyzerInput>.Continuation,
        converting target: AVAudioFormat
    ) throws {
        guard let device = AVCaptureDevice.default(for: .audio) else {
            throw DictationError.noInputDevice
        }

        let session = AVCaptureSession()
        let input = try AVCaptureDeviceInput(device: device)
        guard session.canAddInput(input) else { throw DictationError.noInputDevice }
        session.addInput(input)

        let (levels, levelContinuation) = AsyncStream<Float>.makeStream()
        tasks.append(Task { [weak self] in
            for await level in levels { self?.push(level) }
        })

        let output = AVCaptureAudioDataOutput()
        let pump = AudioPump(
            target: target,
            continuation: continuation,
            levels: levelContinuation
        )
        output.setSampleBufferDelegate(pump, queue: pump.queue)
        guard session.canAddOutput(output) else { throw DictationError.noInputDevice }
        session.addOutput(output)

        self.pump = pump
        self.session = session

        // startRunning blocks, and Apple's guidance is to call it off the main queue. The session
        // is not touched again until stopRunning, and start and stop are documented as safe from
        // any queue, so handing this one reference across is sound. Marked unsafe only because
        // AVCaptureSession carries no Sendable conformance.
        nonisolated(unsafe) let starting = session
        Task.detached { starting.startRunning() }
    }

    private var pump: AudioPump?

    /// Keeps a short rolling window, which is all a waveform needs.
    private func push(_ level: Float) {
        levels.append(level)
        if levels.count > Self.waveformBars {
            levels.removeFirst(levels.count - Self.waveformBars)
        }
    }

    static let waveformBars = 32

    /// Abandons an utterance without flushing. Used when starting failed partway through.
    private func teardown() async {
        endInput()
        levels = []
        await analyzer?.cancelAndFinishNow()
        releaseAnalyzer()
    }
}

enum DictationError: Error {
    case noInputDevice
    case noCompatibleFormat
}

/// Turns capture callbacks into analyzer input.
///
/// Lives outside the main actor because the delegate fires on its own queue. Yielding into an
/// `AsyncStream` is thread-safe, so no hop is needed.
private final class AudioPump: NSObject, AVCaptureAudioDataOutputSampleBufferDelegate {
    let queue = DispatchQueue(label: "dev.sabharish.loki.audio")

    private let target: AVAudioFormat
    private let continuation: AsyncStream<AnalyzerInput>.Continuation
    private let levels: AsyncStream<Float>.Continuation
    private var converter: AVAudioConverter?

    init(
        target: AVAudioFormat,
        continuation: AsyncStream<AnalyzerInput>.Continuation,
        levels: AsyncStream<Float>.Continuation
    ) {
        self.target = target
        self.continuation = continuation
        self.levels = levels
    }

    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        guard let source = Self.pcmBuffer(from: sampleBuffer) else { return }
        levels.yield(Self.loudness(of: source))
        guard let converted = convert(source) else { return }
        continuation.yield(AnalyzerInput(buffer: converted))
    }

    /// Root mean square of the buffer, scaled so ordinary speech fills most of the range.
    private static func loudness(of buffer: AVAudioPCMBuffer) -> Float {
        guard let channel = buffer.floatChannelData?[0], buffer.frameLength > 0 else { return 0 }
        let frames = Int(buffer.frameLength)
        var sum: Float = 0
        for i in 0..<frames {
            sum += channel[i] * channel[i]
        }
        let rms = (sum / Float(frames)).squareRoot()
        // Speech sits well below full scale, so a linear meter barely moves. Compress it.
        return min(1, (rms * 12).squareRoot())
    }

    private func convert(_ source: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        if source.format == target { return source }

        if converter?.inputFormat != source.format {
            converter = AVAudioConverter(from: source.format, to: target)
        }
        guard let converter else { return nil }

        let ratio = target.sampleRate / source.format.sampleRate
        let capacity = AVAudioFrameCount(Double(source.frameLength) * ratio) + 1024
        guard let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else {
            return nil
        }

        let input = OneShotInput(source)
        var error: NSError?
        converter.convert(to: output, error: &error) { _, status in input.next(status) }
        return error == nil && output.frameLength > 0 ? output : nil
    }

    /// Hands a buffer to the converter once, then reports no more data.
    ///
    /// A holder rather than a captured `var`, because the converter's input block is `@Sendable`
    /// and a mutable capture cannot be expressed safely.
    ///
    /// Safety invariant: `AVAudioConverter.convert` calls the block synchronously, on the calling
    /// thread, and returns before it can be called again. One holder is created per conversion
    /// and never escapes that call, so there is no second reference and nothing to race with.
    /// A `Mutex` would not help, since putting a non-Sendable buffer into one is itself a send.
    private final class OneShotInput: @unchecked Sendable {
        private var buffer: AVAudioPCMBuffer?

        init(_ buffer: AVAudioPCMBuffer) {
            self.buffer = buffer
        }

        func next(_ status: UnsafeMutablePointer<AVAudioConverterInputStatus>) -> AVAudioBuffer? {
            guard let buffer else {
                status.pointee = .noDataNow
                return nil
            }
            self.buffer = nil
            status.pointee = .haveData
            return buffer
        }
    }

    private static func pcmBuffer(from sampleBuffer: CMSampleBuffer) -> AVAudioPCMBuffer? {
        guard let description = CMSampleBufferGetFormatDescription(sampleBuffer),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description),
              let format = AVAudioFormat(streamDescription: asbd)
        else { return nil }

        let frames = AVAudioFrameCount(CMSampleBufferGetNumSamples(sampleBuffer))
        guard frames > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)
        else { return nil }

        buffer.frameLength = frames
        let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(
            sampleBuffer,
            at: 0,
            frameCount: Int32(frames),
            into: buffer.mutableAudioBufferList
        )
        return status == noErr ? buffer : nil
    }
}

/// One result from a transcriber, tagged with where in the utterance it belongs.
struct Segment {
    let start: Double
    let text: String
    let isFinal: Bool
}

/// An utterance assembled from segments that arrive out of order and get revised.
///
/// A transcriber does not hand back one growing string. It finalizes the utterance in pieces and
/// keeps revising the piece it is still working on, so each result covers only its own time
/// range. Keying by range start and rebuilding is the only assembly that survives that: taking
/// the newest result as the whole transcript loses every earlier piece, which is how "Hey Loki,
/// how are you?" once ended up in the composer as "?".
struct Transcript {
    private var settled: [Double: String] = [:]
    private var pending: (start: Double, text: String)?

    /// The utterance so far, settled pieces followed by the piece still being revised.
    var text: String {
        var pieces = settled.keys.sorted().map { settled[$0, default: ""] }
        if let pending, settled[pending.start] == nil {
            pieces.append(pending.text)
        }
        // Segments carry their own leading space, so joining adds none and the result is tidied
        // once at the end.
        return pieces
            .joined()
            .split(separator: " ", omittingEmptySubsequences: true)
            .joined(separator: " ")
    }

    mutating func absorb(_ segment: Segment) {
        if segment.isFinal {
            settled[segment.start] = segment.text
            if pending?.start == segment.start {
                pending = nil
            }
        } else {
            pending = (segment.start, segment.text)
        }
    }
}
