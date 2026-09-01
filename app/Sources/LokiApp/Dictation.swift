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
        case unavailable(String)
    }

    private(set) var status: Status = .idle
    /// Text so far this utterance, including the volatile tail.
    private(set) var transcript = ""

    /// Fires the moment speech is detected, before transcription finishes.
    ///
    /// Talking over a running task is an interrupt, and the visible stop has a 150ms budget, so
    /// the interrupt cannot wait for words.
    var onSpeechStart: (() -> Void)?

    private var session: AVCaptureSession?
    private var analyzer: SpeechAnalyzer?
    private var inputContinuation: AsyncStream<AnalyzerInput>.Continuation?
    private var tasks: [Task<Void, Never>] = []
    private var finalized = ""

    var isListening: Bool { status == .listening }

    /// Begins an utterance. Idempotent.
    func start() async {
        guard status == .idle || status == .denied else { return }
        status = .preparing
        transcript = ""
        finalized = ""

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
    @discardableResult
    func stop() async -> String {
        guard status == .listening || status == .preparing else { return "" }
        try? await analyzer?.finalizeAndFinishThroughEndOfInput()
        await teardown()
        status = .idle
        return transcript.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func begin() async throws {
        let locale = Locale.current
        let transcriber = SpeechTranscriber(locale: locale, preset: .transcription)
        let detector = SpeechDetector(detectionOptions: .init(sensitivityLevel: .medium),
                                      reportResults: true)
        let modules: [any SpeechModule] = [transcriber, detector]

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
            guard let results = self?.transcriberResults(transcriber) else { return }
            for await text in results { self?.absorb(text) }
        })
        tasks.append(Task { [weak self] in
            guard let detections = self?.detectorResults(detector) else { return }
            for await detected in detections where detected {
                self?.onSpeechStart?()
            }
        })

        try startCapture(feeding: continuation, converting: format)
    }

    /// Bridges the transcriber's throwing sequence into a plain stream of text.
    private func transcriberResults(
        _ transcriber: SpeechTranscriber
    ) -> AsyncStream<(String, Bool)> {
        AsyncStream { continuation in
            Task {
                do {
                    for try await result in transcriber.results {
                        let text = String(result.text.characters)
                        // A volatile result is revised as more audio arrives. Only a finalized
                        // one is appended, otherwise the draft would stutter.
                        let isFinal = result.resultsFinalizationTime >= result.range.end
                        continuation.yield((text, isFinal))
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

    private func absorb(_ result: (text: String, isFinal: Bool)) {
        if result.isFinal {
            finalized += result.text
            transcript = finalized
        } else {
            transcript = finalized + result.text
        }
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

        let output = AVCaptureAudioDataOutput()
        let pump = AudioPump(target: target, continuation: continuation)
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

    private func teardown() async {
        session?.stopRunning()
        session = nil
        pump = nil
        inputContinuation?.finish()
        inputContinuation = nil
        await analyzer?.cancelAndFinishNow()
        analyzer = nil
        tasks.forEach { $0.cancel() }
        tasks.removeAll()
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
    private var converter: AVAudioConverter?

    init(target: AVAudioFormat, continuation: AsyncStream<AnalyzerInput>.Continuation) {
        self.target = target
        self.continuation = continuation
    }

    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        guard let source = Self.pcmBuffer(from: sampleBuffer) else { return }
        guard let converted = convert(source) else { return }
        continuation.yield(AnalyzerInput(buffer: converted))
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
