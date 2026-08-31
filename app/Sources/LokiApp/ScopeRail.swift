import SwiftUI

/// RAII made visible.
///
/// A rail draws while its scope is open and closes when the resources are released. An unclosed
/// rail is a leaked resource the user notices before we find it in a log.
struct ScopeRail: View {
    let scope: Scope

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Space.m) {
            RoundedRectangle(cornerRadius: 1)
                .fill(scope.state.color)
                .frame(width: Theme.Size.rail)

            VStack(alignment: .leading, spacing: Theme.Space.s) {
                header
                ForEach(scope.steps) { step in
                    HStack(spacing: Theme.Space.s) {
                        Text(step.verb)
                            .font(Theme.Text.meta)
                            .kerning(Theme.Text.metaTracking)
                            .foregroundStyle(Theme.Colors.faint)
                            .frame(width: 48, alignment: .leading)
                        Text(step.detail)
                            .font(Theme.Text.body)
                            .foregroundStyle(Theme.Colors.muted)
                    }
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private var header: some View {
        HStack(spacing: Theme.Space.s) {
            Image(systemName: scope.state.glyph)
                .font(.system(size: 9))
                .foregroundStyle(scope.state.color)
            Text(scope.state.label)
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(scope.state.color)
                .padding(.horizontal, Theme.Space.xs)
                .padding(.vertical, 1)
                .background(scope.state.tint, in: .rect(cornerRadius: Theme.Radius.control))
            Text(scope.kind)
                .font(Theme.Text.body)
                .foregroundStyle(Theme.Colors.ink)
            Spacer(minLength: Theme.Space.m)
            if let elapsed = scope.elapsed {
                Text(format(elapsed))
                    .font(Theme.Text.meta)
                    .monospacedDigit()
                    .foregroundStyle(Theme.Colors.faint)
            }
        }
    }

    private func format(_ ms: UInt64) -> String {
        ms < 1000 ? "\(ms)ms" : String(format: "%.2fs", Double(ms) / 1000)
    }
}
