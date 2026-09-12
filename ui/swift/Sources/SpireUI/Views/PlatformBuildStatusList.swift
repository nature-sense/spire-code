import SwiftUI
import Foundation

/// Per-target ("per platform") last-action state for a subproject's build
/// targets: the last BUILD result (dot + when + duration) plus compact
/// lint/test badges.
///
/// The Rust build manager persists every per-platform action under
/// `<kind>.last.<absPath>.<target>` — `kind` being `build` / `lint` / `test` /
/// `clean` — so this reads each kind back with exactly the same absolute path
/// and target name the action used. A mismatch here would silently show
/// "never built" for a target that has in fact been built.
///
/// This is the left panel's answer to "what is the state and date of the last
/// build for each platform?".
struct PlatformBuildStatusList: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    let project: ProjectInfo
    let subproject: SubprojectInfo
    /// Currently selected target — highlighted row.
    let selectedTarget: String?
    /// Tapping a row selects that target, so the right pane's action row
    /// (Build / Clean / Lint …) then drives that platform.
    var onSelect: (BuildTarget) -> Void

    /// Last-action status per target name, one map per action kind.
    @State private var buildStatuses: [String: BuildStatus] = [:]
    @State private var lintStatuses: [String: BuildStatus] = [:]
    @State private var testStatuses: [String: BuildStatus] = [:]
    @State private var loading = false

    /// Absolute path of the subproject: build status is stored under the
    /// ABSOLUTE path, so the read must use the same form the build used.
    private var absPath: String { subproject.absolutePath(in: project.root) }

    /// Reload trigger: project/subproject identity plus the tick the app bumps
    /// after every completed build action (so the dates refresh immediately).
    private var refreshKey: String {
        "\(project.root)|\(subproject.path)|\(bridge.buildCompletionTick)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text("Platform builds")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(theme.textSecondary)
                if loading {
                    ProgressView().controlSize(.mini)
                }
                Spacer()
            }
            ForEach(subproject.buildTargets) { target in
                row(target)
            }
        }
        .task(id: refreshKey) { await load() }
    }

    @ViewBuilder
    private func row(_ target: BuildTarget) -> some View {
        Button {
            onSelect(target)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: dotSymbol(target))
                    .font(.caption)
                    .foregroundStyle(dotColor(target))
                VStack(alignment: .leading, spacing: 1) {
                    Text(target.name).font(.callout)
                    Text(subtitle(target))
                        .font(.caption2)
                        .foregroundStyle(theme.textSecondary)
                    if let l = lintStatuses[target.name] { badge("lint", l) }
                    if let t = testStatuses[target.name] { badge("test", t) }
                }
                Spacer()
                if target.platform != "host" {
                    Text(target.platform)
                        .font(.caption2)
                        .foregroundStyle(theme.textSecondary)
                }
            }
            .padding(.vertical, 6)
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .rowStyle(selected: selectedTarget == target.name, theme: theme)
        }
        .buttonStyle(.plain)
        .help(helpText(target))
    }

    // MARK: - Presentation

    /// Compact "✓ lint" / "✗ test" chip for a secondary action result.
    private func badge(_ label: String, _ s: BuildStatus) -> some View {
        HStack(spacing: 2) {
            Image(systemName: s.success == true ? "checkmark.circle.fill" : "xmark.circle.fill")
                .font(.system(size: 8))
                .foregroundStyle(s.success == true ? Color.green : Color.red)
            Text(label).font(.system(size: 9))
        }
        .foregroundStyle(theme.textSecondary)
    }

    private func status(_ target: BuildTarget) -> BuildStatus? { buildStatuses[target.name] }

    private func dotSymbol(_ target: BuildTarget) -> String {
        switch status(target)?.success {
        case .some(true):  return "checkmark.circle.fill"
        case .some(false): return "xmark.circle.fill"
        default:           return "circle.dashed"
        }
    }

    private func dotColor(_ target: BuildTarget) -> Color {
        switch status(target)?.success {
        case .some(true):  return .green
        case .some(false): return .red
        default:           return theme.textSecondary
        }
    }

    private func subtitle(_ target: BuildTarget) -> String {
        guard let s = status(target) else { return "never built" }
        var parts = [s.success == true ? "succeeded" : "failed"]
        if let d = s.lastBuild { parts.append(Self.relative(d)) }
        if let dur = s.durationSecs { parts.append(String(format: "%.1fs", dur)) }
        return parts.joined(separator: " · ")
    }

    private func helpText(_ target: BuildTarget) -> String {
        var lines = ["\(target.name) — platform '\(target.platform)'"]
        for (label, s) in [
            ("build", buildStatuses[target.name]),
            ("lint", lintStatuses[target.name]),
            ("test", testStatuses[target.name]),
        ] {
            guard let s else {
                lines.append("  \(label): never run")
                continue
            }
            let when = s.lastBuild.map { Self.absolute($0) } ?? "unknown time"
            let dur = s.durationSecs.map { String(format: " (%.1fs)", $0) } ?? ""
            lines.append("  \(label): \(s.success == true ? "succeeded" : "failed") \(when)\(dur)")
        }
        return lines.joined(separator: "\n")
    }

    private static func relative(_ date: Date) -> String {
        let fmt = RelativeDateTimeFormatter()
        fmt.unitsStyle = .short
        return fmt.localizedString(for: date, relativeTo: Date())
    }

    private static func absolute(_ date: Date) -> String {
        let fmt = DateFormatter()
        fmt.dateFormat = "EEE d MMM HH:mm"
        return fmt.string(from: date)
    }

    // MARK: - Loading

    private func load() async {
        loading = true
        var build: [String: BuildStatus] = [:]
        var lint: [String: BuildStatus] = [:]
        var test: [String: BuildStatus] = [:]
        for target in subproject.buildTargets {
            if let s = await bridge.fetchBuildStatus(path: absPath, target: target.name) {
                build[target.name] = s
            }
            if let s = await bridge.fetchBuildStatus(path: absPath, target: target.name, kind: "lint") {
                lint[target.name] = s
            }
            if let s = await bridge.fetchBuildStatus(path: absPath, target: target.name, kind: "test") {
                test[target.name] = s
            }
        }
        buildStatuses = build
        lintStatuses = lint
        testStatuses = test
        loading = false
    }
}
