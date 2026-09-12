import SwiftUI
import Foundation

/// Per-target ("per platform") last-build state for a subproject's build
/// targets: success dot, target name, and WHEN it was last built.
///
/// The Rust build manager persists build status PER TARGET under
/// `build.last.<absPath>.<target>` (and `build.last.<absPath>` when a build ran
/// with no target selected), so this reads it back with exactly the same
/// absolute path and target name the build action used — any mismatch silently
/// shows "never built" for a target that has in fact been built.
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

    /// Last-build status keyed by target name.
    @State private var statuses: [String: BuildStatus] = [:]
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

    private func status(_ target: BuildTarget) -> BuildStatus? { statuses[target.name] }

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
        guard let s = status(target) else {
            return "\(target.name) — no recorded build for platform '\(target.platform)'"
        }
        let when = s.lastBuild.map { Self.absolute($0) } ?? "unknown time"
        let dur = s.durationSecs.map { String(format: " (%.1fs)", $0) } ?? ""
        return "\(target.name) — \(s.success == true ? "succeeded" : "failed") \(when)\(dur)"
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
        var out: [String: BuildStatus] = [:]
        for target in subproject.buildTargets {
            if let s = await bridge.fetchBuildStatus(path: absPath, target: target.name) {
                out[target.name] = s
            }
        }
        statuses = out
        loading = false
    }
}
