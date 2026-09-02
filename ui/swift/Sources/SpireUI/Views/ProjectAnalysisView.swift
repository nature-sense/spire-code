import SwiftUI

/// Data-dense project analysis pane — replaces the sparse node graph in the
/// top-left region. Uses only data the Rust analyzer already provides on
/// `ProjectInfo` / `SubprojectInfo`:
///   • header: project name (single row, no filler subtitle)
///   • project structure: per build system → languages + subproject count
///     + package description — always visible, no scrolling to see it
///   • subprojects (selectable → drives the center "Subproject" context)
///   • build targets (selectable → drives Sources/Dependencies/Builds)
/// The same selection bindings the graph previously exposed are preserved so
/// the center and right panes keep working unchanged.
struct ProjectAnalysisView: View {
    let project: ProjectInfo
    @Binding var selectedSubproject: SubprojectInfo?
    @Binding var selectedBuildTarget: String?
    /// nil = the project root; non-nil = a HAL domain (common / rpi5 / …).
    /// Only meaningful for HAL Meson projects (structure == "hal").
    @Binding var selectedDomain: String?
    /// Directory the page browses (nil = whole project). Set by layout leaves
    /// and read by the Sources pane so it mirrors the clicked directory.
    @Binding var selectedLayoutDirectory: String?
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    init(project: ProjectInfo,
         selectedSubproject: Binding<SubprojectInfo?>,
         selectedBuildTarget: Binding<String?>,
         selectedDomain: Binding<String?> = .constant(nil),
         selectedLayoutDirectory: Binding<String?> = .constant(nil)) {
        self.project = project
        self._selectedSubproject = selectedSubproject
        self._selectedBuildTarget = selectedBuildTarget
        self._selectedDomain = selectedDomain
        self._selectedLayoutDirectory = selectedLayoutDirectory
    }

    /// "Add platform" picker state: visible, the candidate list (registry
    /// minus already-present), the chosen platform id, and the busy/error state.
    @State private var showAddPlatformSheet = false
    @State private var candidatePlatforms: [Platform] = []
    @State private var selectedNewPlatform: String?
    @State private var isAddingPlatform = false
    @State private var addPlatformError: String?

    /// HAL contract-format lint state for the `hal/api` leaf badge — loaded
    /// once per layout render so the row shows "Contracts OK" or "N issues"
    /// inline; clicking the row surfaces the full correction plan on the right.
    @State private var apiLintReport: HalDocLintReport?
    @State private var apiLintLoading = true

    var body: some View {
        if isHalProject {
            // HAL Meson: a single card — Project row + Platforms (common/rpi5/…).
            // No header/structure/subprojects/build-targets cards: the project
            // row and the platform list carry all the useful information, and
            // the analysis pane fills the vertical space with just the rows.
            VStack(alignment: .leading, spacing: 10) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        platformsCard
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(theme.background)
            .task(id: project.root) {
                // Load the contract-format lint once so the `api` leaf badge
                // carries the OK/issues status inline. Errors leave the badge
                // on "—" (the right pane's card shows the real detail).
                apiLintLoading = true
                apiLintReport = await bridge.halDocLint(root: project.root)
                apiLintLoading = false
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                headerSection
                    .fixedSize(horizontal: false, vertical: true)

                structureSection
                    .fixedSize(horizontal: false, vertical: true)

                // The lower lists are the only parts that may scroll when long.
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        subprojectsSection
                        buildTargetsSection
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(theme.background)
        }
    }

    // MARK: - Project Structure (grouped by build system)

    /// Per build system → the subprojects under it, languages, and first
    /// non-empty description. Drives the structure summary.
    private var structureGroups: [(system: String, subs: [SubprojectInfo])] {
        let subs = project.subprojects.filter { !$0.buildSystem.isEmpty }
        // Preserve project.buildSystems ordering; fall back to insertion order.
        let order = project.buildSystems.isEmpty
            ? subs.map(\.buildSystem).reduce(into: []) { $0.append($1) }
            : project.buildSystems
        var seen = Set<String>()
        var groups: [(String, [SubprojectInfo])] = []
        for system in order where !seen.contains(system) {
            let members = subs.filter { $0.buildSystem == system }
            if !members.isEmpty {
                groups.append((system, members))
                seen.insert(system)
            }
        }
        return groups
    }

    private var structureSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Project Structure")
                .font(.headline)

            if structureGroups.isEmpty {
                Text("No build structure detected yet")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(structureGroups.indices, id: \.self) { idx in
                    let group = structureGroups[idx]
                    let langs = uniqueLanguages(group.subs)
                    let description = group.subs
                        .map(\.description)
                        .first { !$0.isEmpty }

                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: subprojectIcon(group.system))
                            .font(.title3)
                            .foregroundStyle(subprojectColor(group.system))
                            .frame(width: 22)

                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(group.system)
                                    .font(.callout.weight(.semibold))
                                Text("· \(group.subs.count) subproject\(group.subs.count == 1 ? "" : "s")")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            if !langs.isEmpty {
                                Text(langs)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            if let description {
                                Text(description)
                                    .font(.caption)
                                    .foregroundStyle(.tertiary)
                                    .lineLimit(2)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 3)
                }
            }
        }
        .cardStyle(theme: theme)
    }

    /// Distinct language labels (e.g. "🦀 Rust") across a build system's
    /// subprojects, joined with a separator.
    private func uniqueLanguages(_ subs: [SubprojectInfo]) -> String {
        let langs = subs.map(\.language).filter { !$0.isEmpty }
        var seen = Set<String>()
        let distinct = langs.filter { seen.insert($0).inserted }
        return distinct.joined(separator: " · ")
    }

    // MARK: - Header (compact single row)

    private var headerSection: some View {
        HStack(spacing: 8) {
            Image(systemName: "square.grid.2x2")
                .foregroundStyle(theme.accent)
            Text(project.name)
                .font(.title3.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.middle)
            if isSpireAppProject {
                Text("Spire app")
                    .font(.caption.weight(.medium))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(theme.accent.opacity(0.15), in: Capsule())
                    .foregroundStyle(theme.accent)
            }
            Spacer(minLength: 0)
        }
        .cardStyle(theme: theme)
    }

    // MARK: - Subprojects

    private var subprojectsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Subprojects")
                .font(.headline)

            if project.subprojects.isEmpty {
                Text("None")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                let list = project.subprojects.filter { $0.buildSystem.isEmpty == false }
                if list.isEmpty {
                    Text("Not detected yet")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(list) { sub in
                        Button {
                            selectedSubproject = sub
                            selectedBuildTarget = nil
                            bridge.selectSubproject(sub)
                        } label: {
                            HStack(spacing: 8) {
                                Image(systemName: subprojectIcon(sub.buildSystem))
                                    .foregroundStyle(subprojectColor(sub.buildSystem))
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(sub.name).font(.callout.weight(.medium))
                                    if !sub.description.isEmpty {
                                        Text(sub.description)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(2)
                                    }
                                }
                                Spacer()
                                Text(sub.buildSystem)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.vertical, 8)
                            .padding(.horizontal, 8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .rowStyle(selected: selectedSubproject?.id == sub.id, theme: theme)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
        .cardStyle(theme: theme)
    }

    // MARK: - HAL domains (HAL Meson projects only)

    private var isHalProject: Bool {
        project.subprojects.contains { $0.structure == "hal" }
    }

    /// A SpireApp project (Rust/SwiftUI monorepo) — any subproject carries the
    /// `spire_app` structure the Cargo analyzer detects for workspaces that
    /// depend on spire-actor + spire-core and ship a ui/swift companion.
    private var isSpireAppProject: Bool {
        project.subprojects.contains { $0.structure == "spire_app" }
    }

    /// The HAL domains from the root Meson subproject (common / rpi5 / …).
    private var halDomains: [ProjectDomain] {
        project.subprojects.first(where: { !$0.domains.isEmpty })?.domains ?? []
    }

    /// HAL "Project" card — a directory-faithful tree mirroring the real
    /// ai-traps layout: Project / Common (toolkit) / HAL (api + platforms) /
    /// Targets (per-platform executables). Selecting a leaf wires the same
    /// selection bindings the flat card used:
    ///   • project root → clear domain + target
    ///   • toolkit / api → select the `common` domain
    ///   • platform → select that platform domain
    ///   • target → select the Meson build target
    private var platformsCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(ProjectLayout(project: project).root.children) { section in
                LayoutTreeNodeView(
                    node: section,
                    depth: 0,
                    isSelected: { isSelected($0) },
                    onSelect: { select($0) },
                    domainLookup: { domain(for: $0) },
                    apiLint: (loading: apiLintLoading, report: apiLintReport)
                )
            }

            Divider()

            // Project-level action: add a NEW platform from the registry into
            // this project (scaffolds <plat>/, HAL stubs, meson wiring, options).
            Button {
                openAddPlatformSheet()
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "plus.circle")
                        .foregroundStyle(theme.accent)
                    Text("Add platform…")
                        .font(.callout.weight(.medium))
                    Spacer()
                }
                .padding(.vertical, 8)
                .padding(.horizontal, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
        }
        .cardStyle(theme: theme)
        .sheet(isPresented: $showAddPlatformSheet) {
            addPlatformSheet
        }
    }

    /// The domain that a layout node selects (nil for non-domain rows).
    private func domain(for node: ProjectLayout.Node) -> ProjectDomain? {
        guard let id = node.domainId else { return nil }
        return halDomains.first { $0.id == id }
    }

    /// True when the node matches the current selection. Highlighting is keyed
    /// on the node's UNIQUE directory (each leaf browsed in the Sources pane),
    /// NOT the shared analyzer domain id — `toolkit` and `hal/api` both belong
    /// to the `common` domain, so domain-keyed highlighting would select both
    /// at once. When no layout leaf is active (whole project), the project
    /// row is the implicit selection.
    private func isSelected(_ node: ProjectLayout.Node) -> Bool {
        if selectedLayoutDirectory != nil {
            return node.directory == selectedLayoutDirectory
        }
        switch node.kind {
        case .project:
            return selectedDomain == nil && selectedBuildTarget == nil
        default:
            return false
        }
    }

    /// Apply the node's selection: clear/select domain + target like the old
    /// flat card, so the center/right panes keep working unchanged. Also
    /// scopes the Sources pane to the leaf's real directory (toolkit/,
    /// hal/api/, hal/implementations/<plat>/, <plat>/ — nil = whole project).
    private func select(_ node: ProjectLayout.Node) {
        selectedLayoutDirectory = node.directory
        // Clicking `hal/api` surfaces the contract-format correction plan in
        // the right pane; any other selection dismisses it.
        bridge.showHalContractLint = node.kind == .api
        let hal = project.subprojects.first { $0.structure == "hal" }
        switch node.kind {
        case .project:
            selectedDomain = nil
            bridge.selectedDomain = nil
            selectedBuildTarget = nil
            bridge.selectSubproject(hal)
        case .toolkit, .api, .platform:
            guard let d = domain(for: node) else { return }
            selectedDomain = d.id
            bridge.selectedDomain = d.id
            selectedBuildTarget = nil
            bridge.selectSubproject(hal)
        case .target:
            guard let name = node.targetName else { return }
            selectedDomain = nil
            bridge.selectedDomain = nil
            selectedBuildTarget = name
            bridge.selectSubproject(hal)
        default:
            break
        }
    }

    /// "Add platform" picker: candidates = registry platforms minus those
    /// already present in the project (from existing platform domains' names).
    private var addPlatformSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add a platform")
                .font(.title3.weight(.semibold))
            Text("Scaffolds a new target: platform sources, HAL placeholder stubs, meson wiring, and options. Implementations are generated later via the HAL fill workflow.")
                .font(.callout)
                .foregroundStyle(.secondary)

            if addPlatformError != nil {
                Label(addPlatformError ?? "", systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
            }

            if candidatePlatforms.isEmpty {
                Text("No new platforms available in the registry (~/.spire/platforms).")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                Picker("Platform", selection: $selectedNewPlatform) {
                    Text("Select platform…").tag(String?.none)
                    ForEach(candidatePlatforms) { p in
                        Text("\(p.name) (\(p.id))").tag(String?.some(p.id))
                    }
                }
                .frame(maxWidth: 340)
            }

            HStack {
                Spacer()
                Button("Cancel") {
                    showAddPlatformSheet = false
                    addPlatformError = nil
                }
                .keyboardShortcut(.cancelAction)

                Button {
                    addPlatform()
                } label: {
                    if isAddingPlatform {
                        ProgressView().scaleEffect(0.7)
                    } else {
                        Label("Add Platform", systemImage: "plus.circle")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isAddingPlatform || selectedNewPlatform == nil)
            }
        }
        .padding(20)
        .frame(width: 480)
    }

    // MARK: - Build targets

    private var buildTargetsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Build Targets")
                .font(.headline)

            let sub = selectedSubproject ?? project.subprojects.first { $0.buildSystem.isEmpty == false }
            if let sub, !sub.buildTargets.isEmpty {
                let targets = sub.buildTargets
                ForEach(targets) { target in
                    Button {
                        selectedBuildTarget = target.name
                        selectedSubproject = sub
                        bridge.selectSubproject(sub)
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "gearshape.fill")
                                .font(.caption)
                                .foregroundStyle(.orange)
                            Text(target.name).font(.callout)
                            Spacer()
                            if target.platform != "host" {
                                Text(target.platform)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(.vertical, 8)
                        .padding(.horizontal, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .rowStyle(selected: selectedBuildTarget == target.name, theme: theme)
                    }
                    .buttonStyle(.plain)
                }
            } else if let sub {
                Text("No targets — build the whole subproject")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Text("None")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .cardStyle(theme: theme)
    }

    // MARK: - Add platform

    /// Open the picker: fetch registry platforms, filter to those NOT already
    /// present in the project (the platform domain `name`s are the ids).
    private func openAddPlatformSheet() {
        addPlatformError = nil
        selectedNewPlatform = nil
        candidatePlatforms = []
        Task {
            let all = await bridge.fetchPlatforms()
            let existingNames = Set(halDomains.map(\.name))
            let candidates = all
                .filter { !existingNames.contains($0.id) }
                .sorted { $0.name < $1.name }
            await MainActor.run {
                candidatePlatforms = candidates
                showAddPlatformSheet = true
            }
        }
    }

    /// Run `hal_add_platform` for the chosen platform, then refresh the project
    /// analysis + HAL coverage so the new domain/build-target appears live.
    private func addPlatform() {
        guard let platform = selectedNewPlatform, let root = bridge.projectRoot else { return }
        isAddingPlatform = true
        addPlatformError = nil
        Task {
            let result = await bridge.halAddPlatform(root: root, platform: platform)
            await MainActor.run {
                isAddingPlatform = false
                if let err = result.error {
                    addPlatformError = err
                } else {
                    showAddPlatformSheet = false
                    candidatePlatforms = []
                    // Refresh the project tree + per-platform HAL coverage so
                    // the new platform domain and its build target appear.
                    Task {
                        await bridge.fetchProjectAnalysis(projectRoot: root)
                        await bridge.refreshHalData(root: root)
                    }
                }
            }
        }
    }

    // MARK: - Helpers

    private func subprojectIcon(_ bs: String) -> String {
        switch bs {
        case "Cargo": return "gearshape.fill"
        case "SwiftPM", "Xcode": return "hammer.fill"
        case "npm", "pnpm", "yarn": return "square.and.pencil"
        default: return "folder.fill"
        }
    }

    private func subprojectColor(_ bs: String) -> Color {
        switch bs {
        case "Cargo": return Color(red: 0.70, green: 0.25, blue: 0.15)
        case "SwiftPM", "Xcode": return .orange
        case "npm", "pnpm", "yarn": return .green
        default: return .gray
        }
    }
}

/// One row in the directory-faithful HAL layout tree (ProjectLayout). Non-leaf
/// nodes render a DisclosureGroup section header; leaves render a selectable
/// row that wires the domain/build-target selection the flat card used.
private struct LayoutTreeNodeView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let node: ProjectLayout.Node
    let depth: Int
    /// Per-node selection lookup (persisted in the parent view).
    let isSelected: (ProjectLayout.Node) -> Bool
    /// Applies the node's selection in the parent view.
    let onSelect: (ProjectLayout.Node) -> Void
    /// Resolves the analyzer `ProjectDomain` behind a leaf node (nil for
    /// target/project rows) — used for HAL coverage + dependency badges.
    let domainLookup: (ProjectLayout.Node) -> ProjectDomain?
    /// HAL contract-format lint for the api row's inline OK/issues badge.
    let apiLint: (loading: Bool, report: HalDocLintReport?)

    var body: some View {
        if node.children.isEmpty {
            leafRow
                .padding(.leading, CGFloat(depth) * 14)
        } else {
            DisclosureGroup {
                ForEach(node.children) { child in
                    LayoutTreeNodeView(node: child, depth: depth + 1,
                                       isSelected: isSelected, onSelect: onSelect,
                                       domainLookup: domainLookup, apiLint: apiLint)
                }
            } label: {
                sectionLabel
            }
        }
    }

    /// Section container row (Project / Common / HAL / Targets).
    private var sectionLabel: some View {
        HStack(spacing: 6) {
            Image(systemName: sectionIcon)
                .foregroundStyle(theme.accent)
            Text(node.label)
                .font(.callout.weight(.semibold))
                .foregroundStyle(theme.textPrimary)
        }
        .padding(.vertical, 2)
    }

    /// Leaf row — the selectable domain/target entry.
    private var leafRow: some View {
        Button {
            onSelect(node)
        } label: {
            HStack(spacing: 6) {
                Image(systemName: leafIcon)
                    .foregroundStyle(leafColor)
                Text(node.label)
                    .font(.callout)
                    .foregroundStyle(isSelected(node) ? theme.accent : theme.textPrimary)
                Spacer()
                leafBadge
            }
            .padding(.vertical, 6)
            .padding(.horizontal, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .rowStyle(selected: isSelected(node), theme: theme)
        }
        .buttonStyle(.plain)
    }

    @ViewBuilder
    private var leafBadge: some View {
        switch node.kind {
        case .project:
            Text("Meson · HAL")
                .font(.caption2)
                .foregroundStyle(.secondary)
        case .toolkit:
            // Module count on the shared toolkit/common row.
            Text("HAL \(bridge.halInterfaces.count) modules")
                .font(.caption2)
                .foregroundStyle(.secondary)
        case .api:
            // Inline contract-format validation: OK (checkmark), issues
            // (count), loading (—). Clicking the row shows the correction
            // plan in the right pane.
            if apiLint.loading {
                ProgressView().controlSize(.mini)
            } else if let report = apiLint.report {
                let issueCount = report.files.reduce(0) { $0 + $1.issues.count }
                if issueCount == 0 {
                    Label("Contracts OK", systemImage: "checkmark.circle.fill")
                        .font(.caption2)
                        .foregroundStyle(.green)
                } else {
                    Label("\(issueCount) issue\(issueCount == 1 ? "" : "s")", systemImage: "exclamationmark.triangle.fill")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }
            } else {
                Text("\(node.contracts.count) contracts")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        case .platform:
            // Per-platform HAL MATURITY (AST-level). The core keys coverage by
            // the PLATFORM ID which equals the domain's `name` (rpi5/rock3c);
            // the Swift `id` is the synthetic "domain-platform-rpi5" and would
            // miss. Each interface is classified implemented / stub / partial /
            // missing; the chips show the aggregate at a glance.
            if let domain = domainLookup(node) {
                let gaps = bridge.halFunctionGaps[domain.name] ?? [:]
                HStack(spacing: 4) {
                    let maturities = gaps.values.map(\.maturity)
                    let implCount = maturities.filter { $0 == "implemented" }.count
                    let stubCount = maturities.filter { $0 == "stub" }.count
                    let partialCount = maturities.filter { $0 == "partial" }.count
                    let missingCount = maturities.filter { $0 == "missing" }.count
                    if implCount > 0 {
                        Text("\(implCount)✓")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.green)
                    }
                    if stubCount > 0 {
                        Text("\(stubCount) stub")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.blue)
                    }
                    if partialCount > 0 {
                        Text("\(partialCount) partial")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.orange)
                    }
                    if missingCount > 0 {
                        Text("\(missingCount) missing")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.red)
                    }
                }
                if !domain.dependencies.isEmpty {
                    Text("· \(domain.dependencies.count) deps")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
        case .target:
            if let targetName = node.targetName {
                Text(targetName)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        default:
            EmptyView()
        }
    }

    private var sectionIcon: String {
        switch node.kind {
        case .project: return "square.grid.2x2"
        case .common: return "shippingbox"
        case .hal: return "cpu"
        case .target: return "hammer"
        default: return "folder"
        }
    }

    private var leafIcon: String {
        switch node.kind {
        case .project: return "square.grid.2x2"
        case .toolkit: return "square.stack.3d.up"
        case .api: return "doc.text"
        case .platform: return "cpu"
        case .target: return "hammer"
        default: return "doc"
        }
    }

    private var leafColor: Color {
        switch node.kind {
        case .toolkit: return theme.accent
        case .platform: return .orange
        case .target: return .blue
        default: return theme.accent
        }
    }
}

// MARK: - Shared card styling

/// The darker "card" treatment used by the analysis pane sections:
/// rounded `theme.surface` fill + subtle border + padding.
extension View {
    func cardStyle(theme: AppTheme) -> some View {
        self
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
    }

    /// Selectable row inside a card: darker highlight when selected.
    func rowStyle(selected: Bool, theme: AppTheme) -> some View {
        self
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(selected ? theme.accentBackground : theme.nodeBackground)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(selected ? theme.accent : theme.border, lineWidth: 1)
            )
    }
}