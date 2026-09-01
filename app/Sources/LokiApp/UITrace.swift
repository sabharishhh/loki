import Foundation

/// Appends a line to `/tmp/loki-ui.log` when `LOKI_TRACE_UI` is set. No-op otherwise.
///
/// Exists to answer "did this code path run" without a debugger attached, which a menu bar
/// popover makes awkward.
func uiTrace(_ message: @autoclosure () -> String) {
    guard UITrace.enabled else { return }
    UITrace.write(message())
}

private enum UITrace {
    static let enabled = ProcessInfo.processInfo.environment["LOKI_TRACE_UI"] != nil
    private static let path = "/tmp/loki-ui.log"
    private static let lock = NSLock()

    static func write(_ message: String) {
        lock.lock()
        defer { lock.unlock() }
        let line = "\(Date().timeIntervalSince1970) \(message)\n"
        guard let data = line.data(using: .utf8) else { return }
        if let handle = FileHandle(forWritingAtPath: path) {
            handle.seekToEndOfFile()
            handle.write(data)
            try? handle.close()
        } else {
            try? data.write(to: URL(fileURLWithPath: path))
        }
    }
}
