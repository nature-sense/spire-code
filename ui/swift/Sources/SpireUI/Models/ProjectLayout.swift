import Foundation

/// The left pane's project tree, derived from the analyzer's flat `ProjectInfo`.
/// Three layers, one selection each:
///
///   Project                          → the whole project
///   Common  → Toolkit · Hal Contract → the shared source + the contracts
///   Targets → rpi5 · rock3c · a7s    → THE platform (build + HAL + device)
///
/// Pure Swift derivation — no Rust/transport changes.
struct ProjectLayout {
    enum Kind: String, Equatable {
        case project, common, toolkit, hal, api, platform, target
    }

    struct Node: Identifiable {
        var id: String {
            "\(kind.rawValue)-\(label)-\(targetName ?? "")-\(domainId ?? "")"
        }
        let kind: Kind
        let label: String
        var files: [String] = []
        var contracts: [String] = []
        /// Real Meson build-target name (e.g. "ai-trap-rpi5") — used to set
        /// `selectedBuildTarget`. Travels with the platform-labeled target row.
        var targetName: String?
        /// The analyzer's `ProjectDomain.id` this node selects (matches
        /// `ProjectDomain.id`, e.g. "domain-platform-rpi5").
        var domainId: String?
        /// Real directory this leaf browses in the Sources pane (project-root
        /// relative: "toolkit", "hal/api", "hal/implementations/rpi5", "rpi5").
        /// nil = the whole project.
        var directory: String?
        var children: [Node] = []
        var isLeaf: Bool { children.isEmpty }
    }

    /// Top-level tree: Project → [Project, Common, HAL, Targets].
    let root: Node

    init(project: ProjectInfo) {
        var root = Node(kind: .project, label: project.name)
        var children: [Node] = []

        let halSub = project.subprojects.first { !$0.domains.isEmpty }
            ?? project.subprojects.first { !$0.buildTargets.isEmpty }

        if let halSub {
            // ── Project: the project root row ──
            children.append(Node(kind: .project, label: "Project",
                                 children: [Node(kind: .project, label: project.name)]))

            // ── Common: the shared layer — toolkit source + contract headers ──
            // Both are shared by every platform, so they sit under one heading:
            // `Toolkit` is the reusable source slice, `Hal Contract` is the
            // interface every platform implements against.
            let common = halSub.domains.first { $0.kind == "common" }
            var commonChildren: [Node] = []
            if let common {
                // The analyzer names the shared slice "Common"; the real
                // directory is `toolkit/`. Prefer the directory name when
                // the common domain actually lists the toolkit path.
                let toolkitLabel = common.files.contains { $0.contains("toolkit") }
                    ? "Toolkit" : common.name
                let toolkitDir = common.files.first { $0.contains("toolkit") }
                    ?? "toolkit"
                commonChildren.append(Node(kind: .toolkit, label: toolkitLabel,
                                           files: common.files, contracts: common.contracts,
                                           domainId: common.id, directory: toolkitDir))
                if !common.contracts.isEmpty {
                    commonChildren.append(Node(kind: .api, label: "Hal Contract",
                                               files: common.contracts,
                                               contracts: common.contracts,
                                               domainId: common.id,
                                               directory: "hal/api"))
                }
            }
            if !commonChildren.isEmpty {
                children.append(Node(kind: .common, label: "Common", children: commonChildren))
            }

            // ── Targets: the platform layer — one row per platform ──
            //
            // That row IS the platform selection, so it carries both halves:
            // the Meson build target (build/lint/test, and the board's MCP
            // group) and the platform's HAL domain (its implementation). They
            // used to be two rows with different parents that looked alike,
            // which is what made this pane confusing.
            let platformDomains = halSub.domains.filter { $0.kind == "platform" }
            // Each platform builds TWO Meson targets: the app executable
            // (ai-trap-rpi5) and its test harness (ai-traps-rpi5-tests). Both
            // carry the same `platform`, so listing every target gave two rows
            // per platform. A platform row is the APP; the test harness is what
            // the Device group's "Run tests on board" uploads and runs, so it is
            // not a second platform.
            let targets = halSub.buildTargets.filter {
                $0.platform != "host" && !$0.name.hasSuffix("-tests")
            }
            if !targets.isEmpty {
                let targetRows = targets.map { t -> Node in
                    // Label = the platform (rpi5/rock3c); targetName keeps the
                    // real Meson target (ai-trap-rpi5) for builds.
                    let platform = t.platform.isEmpty ? t.name : t.platform
                    let domain = platformDomains.first { $0.name == platform }
                    // Browse the platform's HAL implementation: that is the work
                    // a platform selection leads to. The app glue under
                    // `<plat>/` is Meson wiring, reachable from the project row.
                    let implDir = domain?.files.first { $0.contains("implementations/") }
                        ?? (domain.map { "hal/implementations/\($0.name)" } ?? platform)
                    return Node(kind: .target, label: platform,
                                targetName: t.name,
                                domainId: domain?.id,
                                directory: implDir)
                }
                children.append(Node(kind: .target, label: "Targets", children: targetRows))
            }
        }

        root.children = children
        self.root = root
    }
}