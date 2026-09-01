import Foundation

/// Directory-faithful project layout, derived from the analyzer's flat
/// `ProjectInfo` so the UI follows the real tree shape of a HAL/Meson project:
///
///   Project   ai-traps
///   Common    toolkit
///   HAL       api (contracts) · rpi5 · rock3c
///   Targets   rpi5 · rock3c
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

            // ── Common: shared toolkit source slice ──
            let common = halSub.domains.first { $0.kind == "common" }
            if let common {
                // The analyzer names the shared slice "Common"; the real
                // directory is `toolkit/`. Prefer the directory name when
                // the common domain actually lists the toolkit path.
                let label = common.files.contains { $0.contains("toolkit") }
                    ? "toolkit" : common.name
                let toolkitDir = common.files.first { $0.contains("toolkit") }
                    ?? "toolkit"
                let toolkit = Node(kind: .toolkit, label: label,
                                   files: common.files, contracts: common.contracts,
                                   domainId: common.id, directory: toolkitDir)
                children.append(Node(kind: .common, label: "Common", children: [toolkit]))
            }

            // ── HAL: api (contract headers) + one platform per domain ──
            let platforms = halSub.domains.filter { $0.kind == "platform" }
            if !platforms.isEmpty || !(common?.contracts.isEmpty ?? true) {
                var halChildren: [Node] = []
                // api = the contract headers (hal/api/*.hpp) carried by the
                // common domain. Selecting it scopes to the shared slice that
                // owns the contracts, exactly like selecting `common`.
                if let common, !common.contracts.isEmpty {
                    halChildren.append(Node(kind: .api, label: "api",
                                            files: common.contracts,
                                            contracts: common.contracts,
                                            domainId: common.id,
                                            directory: "hal/api"))
                }
                for p in platforms {
                    let implDir = p.files.first { $0.contains("implementations/") }
                        ?? "hal/implementations/\(p.name)"
                    halChildren.append(Node(kind: .platform, label: p.name,
                                            files: p.files, contracts: p.contracts,
                                            domainId: p.id, directory: implDir))
                }
                children.append(Node(kind: .hal, label: "HAL", children: halChildren))
            }

            // ── Targets: one row per composite platform executable ──
            let targets = halSub.buildTargets.isEmpty
                ? halSub.buildTargets
                : halSub.buildTargets.filter { $0.platform != "host" }
            if !targets.isEmpty {
                let targetRows = targets.map { t -> Node in
                    // Label = the platform directory (rpi5/rock3c); targetName
                    // keeps the real Meson target (ai-trap-rpi5) for builds.
                    // The directory is the TOP-LEVEL platform dir the target
                    // compiles its app sources from (NOT the HAL impls).
                    let dir = t.platform.isEmpty ? t.name : t.platform
                    return Node(kind: .target, label: dir,
                                targetName: t.name, directory: dir)
                }
                children.append(Node(kind: .target, label: "Targets", children: targetRows))
            }
        }

        root.children = children
        self.root = root
    }
}