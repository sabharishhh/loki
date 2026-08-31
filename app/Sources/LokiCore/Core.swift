import CLoki
import Foundation

/// Swift side of the bridge.
///
/// Wraps the C ABI so the rest of the app never touches raw pointers. Every function that
/// returns a string from Rust frees it here, so ownership never leaks into the UI layer.
public enum Core {
    /// The linked core's version. Proves the Rust static library is present and callable.
    public static var version: String {
        guard let ptr = loki_version() else { return "unavailable" }
        defer { loki_string_free(ptr) }
        return String(cString: ptr)
    }
}
