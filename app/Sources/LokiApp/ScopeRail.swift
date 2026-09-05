import SwiftUI

/// RAII made visible.
///
/// A rail draws while its scope is open and closes when the resources are released. An unclosed
/// rail is a leaked resource the user notices before we find it in a log.
///
/// Nested scopes indent under their parent, so a code-mode script's calls read as its own steps
/// rather than as a flat list (§13.3). That is what stops code mode buying tokens with legibility.
struct ScopeRail: View {
    let scope: Scope

    /// How far one level of nesting steps in. Wide enough to read as a hierarchy, narrow enough
    /// that four levels still fit the measure.
    private static let indent: CGFloat = 14

    var body: some View {
        HStack(alignment: .top, spacing: Theme.Space.m) {
            RoundedRectangle(cornerRadius: 1)
                .fill(scope.state.color)
                .frame(width: Theme.Size.rail)

            VStack(alignment: .leading, spacing: Theme.Space.s) {
                header
                ForEach(scope.steps) { step in
                    StepRow(step: step)
                }
                if scope.interrupted {
                    CutMark()
                }
            }
        }
        .padding(.leading, CGFloat(scope.depth) * Self.indent)
        .fixedSize(horizontal: false, vertical: true)
        .animation(Theme.Motion.control, value: scope.state)
        .animation(Theme.Motion.control, value: scope.steps.count)
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
                .foregroundStyle(Theme.Colors.primary)
            Spacer(minLength: Theme.Space.m)
            if let elapsed = scope.elapsed {
                Text(format(elapsed))
                    .font(Theme.Text.meta)
                    .monospacedDigit()
                    .foregroundStyle(Theme.Colors.tertiary)
                    .contentTransition(.numericText())
            }
        }
    }

    private func format(_ ms: UInt64) -> String {
        ms < 1000 ? "\(ms)ms" : String(format: "%.2fs", Double(ms) / 1000)
    }
}

/// One step, with its output collapsed behind a control.
///
/// Tool output goes to a well rather than into the thread. §10.5 puts it in a file and lets only
/// the relevant slice into context; the same reasoning applies to the eye.
private struct StepRow: View {
    let step: Step

    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            HStack(spacing: Theme.Space.s) {
                Text(step.verb)
                    .font(Theme.Text.meta)
                    .kerning(Theme.Text.metaTracking)
                    .foregroundStyle(Theme.Colors.tertiary)
                    .frame(width: 48, alignment: .leading)
                Text(step.detail)
                    .font(Theme.Text.body)
                    .foregroundStyle(Theme.Colors.secondary)

                if step.output != nil {
                    Button {
                        expanded.toggle()
                    } label: {
                        Image(systemName: "chevron.right")
                            .font(.system(size: 8, weight: .semibold))
                            .foregroundStyle(Theme.Colors.tertiary)
                            .rotationEffect(.degrees(expanded ? 90 : 0))
                            .frame(width: 14, height: 14)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .help(expanded ? "Hide output" : "Show output")
                }
                Spacer(minLength: 0)
            }

            if expanded, let output = step.output {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(output)
                        .font(Theme.Text.code)
                        .foregroundStyle(Theme.Colors.secondary)
                        .textSelection(.enabled)
                        .padding(Theme.Space.s)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.Colors.surfaceAlt, in: .rect(cornerRadius: Theme.Radius.control))
                .padding(.leading, 48 + Theme.Space.s)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .animation(Theme.Motion.control, value: expanded)
    }
}

/// Where a turn was cut short (§18.3).
///
/// A mark rather than a disappearance. What was kept and what was dropped is the thing a user
/// needs after interrupting, and a rail that simply stops answers neither.
private struct CutMark: View {
    var body: some View {
        HStack(spacing: Theme.Space.s) {
            ZigZag()
                .stroke(Theme.Colors.border, lineWidth: 1)
                .frame(height: 5)
                .frame(maxWidth: 120)
            Text("stopped here")
                .font(Theme.Text.micro)
                .kerning(Theme.Text.microTracking)
                .foregroundStyle(Theme.Colors.tertiary)
            Spacer(minLength: 0)
        }
        .padding(.top, Theme.Space.xs)
        .transition(.opacity)
    }
}

/// A torn edge. Reads as a cut without needing a label to say so.
private struct ZigZag: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        let step: CGFloat = 5
        var x: CGFloat = rect.minX
        var up = true
        path.move(to: CGPoint(x: x, y: rect.midY))
        while x < rect.maxX {
            x += step
            path.addLine(to: CGPoint(x: min(x, rect.maxX), y: up ? rect.minY : rect.maxY))
            up.toggle()
        }
        return path
    }
}
