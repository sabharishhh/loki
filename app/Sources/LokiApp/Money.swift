import Foundation

/// Formats spend for display.
///
/// Amounts arrive in millionths of a cent, because one model call costs a fraction of a cent and
/// whole cents would round most of them to nothing.
enum Money {
    private static let microCentsPerCent: UInt64 = 1_000_000

    /// A short figure for a status line: `$0.34`, or `<$0.01` for anything not yet a whole cent.
    ///
    /// Never rounds a real cost down to `$0.00`, which would read as free.
    static func short(_ microCents: UInt64) -> String {
        if microCents == 0 { return "$0.00" }
        let cents = microCents / microCentsPerCent
        if cents == 0 { return "<$0.01" }
        return String(format: "$%.2f", Double(cents) / 100)
    }
}
