import Foundation

/// Top-level project information from the Rust project analyzer.
struct ProjectInfo: Codable, Identifiable {
    var id: String { name }
    let name: String
    let root: String
    let languages: [String: Int]      // "Rust" → file count
    let buildSystems: [String]        // ["Cargo", "SwiftPM"]
    let architecture: String
    let subprojects: [SubprojectInfo]
    var fileTree: FileTreeDirectory?

    /// True when the project has no buildable content yet. The analyzer
    /// synthesizes subprojects with empty buildSystem for a fresh/empty
    /// directory (or only top-level directory entries), so this checks for
    /// at least one subproject carrying a real build system.
    var isEmpty: Bool {
        !subprojects.contains { !$0.buildSystem.isEmpty }
    }

    enum CodingKeys: String, CodingKey {
        case name, root, languages, buildSystems = "buildSystems",
             architecture, subprojects, fileTree = "fileTree"
    }


    mutating func apply(event: SpireBridge.FileChangeEvent) {
        // Event paths from the file-watcher are absolute (e.g.
        // /Users/steve/proj/src/main.rs) but the tree stores relative
        // paths (src/main.rs). Strip the project root prefix first so
        // nodes are inserted at the correct tree depth instead of doubling.
        var relativeEvent = event
        let prefix = root.hasSuffix("/") ? root : root + "/"
        if relativeEvent.path.hasPrefix(prefix) {
            relativeEvent.path = String(relativeEvent.path.dropFirst(prefix.count))
        }
        // The watcher event carries no file/directory marker, so resolve it
        // against the real filesystem. This prevents extension-less files
        // (Makefile, LICENSE, Dockerfile, …) from being misread as phantom
        // empty directories.
        if relativeEvent.isDirectory == nil && !relativeEvent.path.isEmpty {
            let absolute = prefix + relativeEvent.path
            var isDir: ObjCBool = false
            relativeEvent.isDirectory = FileManager.default.fileExists(atPath: absolute, isDirectory: &isDir)
                ? isDir.boolValue
                : false
        }
        fileTree?.apply(event: relativeEvent)
    }
}

/// A build target within a subproject (e.g. Meson `executable('myapp-rpi', …)`).
struct BuildTarget: Codable, Identifiable, Hashable {
    var id: String { name }
    let name: String
    let kind: [String]
    /// Cross-compilation platform this target builds for ("host" default).
    let platform: String
    /// Single = one source set, varying build config per platform (Cargo).
    /// Composite = shared + platform/app sources compiled together (Meson).
    let sourceKind: SourceKind
    /// Explicit source composition for composite targets (roles: app/shared/platform).
    let sourceUnits: [SourceUnit]

    enum CodingKeys: String, CodingKey {
        case name, kind, platform = "platform", sourceKind = "sourceKind", sourceUnits = "sourceUnits"
    }

    init(name: String, kind: [String], platform: String = "host",
         sourceKind: SourceKind = .single, sourceUnits: [SourceUnit] = []) {
        self.name = name
        self.kind = kind
        self.platform = platform
        self.sourceKind = sourceKind
        self.sourceUnits = sourceUnits
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        kind = try c.decodeIfPresent([String].self, forKey: .kind) ?? []
        platform = try c.decodeIfPresent(String.self, forKey: .platform) ?? "host"
        sourceKind = try c.decodeIfPresent(SourceKind.self, forKey: .sourceKind) ?? .single
        sourceUnits = try c.decodeIfPresent([SourceUnit].self, forKey: .sourceUnits) ?? []
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(name, forKey: .name)
        try c.encode(kind, forKey: .kind)
        try c.encode(platform, forKey: .platform)
        try c.encode(sourceKind, forKey: .sourceKind)
        try c.encode(sourceUnits, forKey: .sourceUnits)
    }
}

/// Whether a build target's sources are shared across variants or composed.
enum SourceKind: String, Codable, Hashable {
    case single
    case composite
}

/// One source group within a composite build target.
struct SourceUnit: Codable, Hashable {
    /// "app", "shared", or "platform".
    let role: String
    /// Relative to the build module root (e.g. "toolkit/src", "rpi5/src").
    let path: String
    let language: String

    enum CodingKeys: String, CodingKey {
        case role, path, language
    }

    init(role: String, path: String, language: String = "") {
        self.role = role
        self.path = path
        self.language = language
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        role = try c.decodeIfPresent(String.self, forKey: .role) ?? ""
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        language = try c.decodeIfPresent(String.self, forKey: .language) ?? ""
    }
}

struct SubprojectInfo: Codable, Identifiable {
    /// **Identity is the path, not the name.**
    ///
    /// Two directories in different places can share a leaf name — `components/ramen/test` and
    /// `components/actors/test` are both `test` — and an id that collides is two rows SwiftUI cannot
    /// tell apart: `ForEach` gets a duplicate id (a row it may drop), and every selection comparison
    /// in the UI is by this, so one click highlights *both*. The analyzer no longer reports nested
    /// files as subprojects, which is the real fix; this is the other half of it — identity that
    /// cannot collide, whatever the tree looks like. The root subproject's path is empty, and it is
    /// the only one, so it is identity enough.
    var id: String { path }
    let name: String
    let kind: SubprojectKind
    let buildSystem: String
    let path: String
    let language: String              // "🦀 Rust", "🐦 Swift"
    /// Project description from Cargo.toml / package.json ("" if absent).
    let description: String
    let files: [FileEntry]?
    let dependencies: [Dependency]?
    let buildStatus: BuildStatus?
    /// Cross-platform build targets from the analyzer (e.g. ["host", "rpi5"]).
    /// Empty when the subproject is single-platform.
    let platformTargets: [String]
    /// Executable/library targets from the analyzer (e.g. Meson
    /// `executable('myapp-rpi', …)`). Empty when the subsystem has no
    /// per-target build selection.
    let buildTargets: [BuildTarget]
    /// Structural shape: "native" | "single_source" | "hal" | "embedded" | "embedded_app".
    ///
    /// A workspace's members **inherit the workspace's structure**, so a container's crates report
    /// `"embedded"` and an application's crate `"embedded_app"` — which is what lets a per-structure
    /// action surface (the container's board/driver actions) show for the crate and not only for the
    /// workspace. Decodes as `"native"` when the analyzer classified nothing.
    let structure: String
    /// Named domains (common / rpi5 / rock3c). Empty when the shape is native.
    let domains: [ProjectDomain]
    /// What an ESP-IDF **component** says it is — `"driver"` (one device on one bus) or `"library"`
    /// (pure code: an algorithm, a filter, a codec).
    ///
    /// **Sent by the analyzer, not derived here**: the component states the kind in its own
    /// `CMakeLists.txt`, and a second copy of that reading in Swift could only ever disagree with the
    /// first. `nil` for a subproject that is not a component, and for a component that states no kind
    /// (one written by hand) — which the UI shows as no kind at all rather than guessing one.
    let componentKind: String?

    /// The **framework** component this is, when it is one — `"toolkit"`, `"ramen"` or `"actors"` —
    /// and `nil` for everything else.
    ///
    /// **Sent by the analyzer**, because the list of shipped components is the *tool's* (it is what
    /// `FRAMEWORK_FILES` emits and what the edit and remove paths refuse), and a second copy here
    /// could only drift from it. A framework component is not a stub: it arrives complete, nothing
    /// writes it, and nothing removes it — which is what `nil`/not-`nil` decides in the row.
    let componentFramework: String?

    enum CodingKeys: String, CodingKey {
        case name, kind, componentKind = "componentKind",
             componentFramework = "componentFramework",
             buildSystem = "buildSystem",
             path, language, files, dependencies, buildStatus = "buildStatus",
             descriptionKey = "description", platformTargets = "platformTargets",
             buildTargets = "buildTargets", structure = "structure",
             domains = "domains"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        kind = try c.decodeIfPresent(SubprojectKind.self, forKey: .kind) ?? .unknown
        buildSystem = try c.decodeIfPresent(String.self, forKey: .buildSystem) ?? ""
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        language = try c.decodeIfPresent(String.self, forKey: .language) ?? ""
        description = try c.decodeIfPresent(String.self, forKey: .descriptionKey) ?? ""
        files = try c.decodeIfPresent([FileEntry].self, forKey: .files)
        dependencies = try c.decodeIfPresent([Dependency].self, forKey: .dependencies)
        buildStatus = try c.decodeIfPresent(BuildStatus.self, forKey: .buildStatus)
        platformTargets = try c.decodeIfPresent([String].self, forKey: .platformTargets) ?? []
        buildTargets = try c.decodeIfPresent([BuildTarget].self, forKey: .buildTargets) ?? []
        structure = try c.decodeIfPresent(String.self, forKey: .structure) ?? "native"
        domains = try c.decodeIfPresent([ProjectDomain].self, forKey: .domains) ?? []
        componentKind = try c.decodeIfPresent(String.self, forKey: .componentKind)
        componentFramework = try c.decodeIfPresent(String.self, forKey: .componentFramework)
    }

    init(name: String, kind: SubprojectKind, buildSystem: String, path: String,
         language: String, description: String = "", files: [FileEntry]? = nil,
         dependencies: [Dependency]? = nil, buildStatus: BuildStatus? = nil,
         platformTargets: [String] = [], buildTargets: [BuildTarget] = [],
         structure: String = "native", domains: [ProjectDomain] = [],
         componentKind: String? = nil, componentFramework: String? = nil) {
        self.name = name
        self.kind = kind
        self.buildSystem = buildSystem
        self.path = path
        self.language = language
        self.description = description
        self.files = files
        self.dependencies = dependencies
        self.buildStatus = buildStatus
        self.platformTargets = platformTargets
        self.buildTargets = buildTargets
        self.structure = structure
        self.domains = domains
        self.componentKind = componentKind
        self.componentFramework = componentFramework
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(name, forKey: .name)
        try c.encode(kind, forKey: .kind)
        try c.encode(buildSystem, forKey: .buildSystem)
        try c.encode(path, forKey: .path)
        try c.encode(language, forKey: .language)
        try c.encode(description, forKey: .descriptionKey)
        try c.encodeIfPresent(files, forKey: .files)
        try c.encodeIfPresent(dependencies, forKey: .dependencies)
        try c.encodeIfPresent(buildStatus, forKey: .buildStatus)
        try c.encode(platformTargets, forKey: .platformTargets)
        try c.encode(buildTargets, forKey: .buildTargets)
        try c.encode(structure, forKey: .structure)
        try c.encode(domains, forKey: .domains)
        try c.encodeIfPresent(componentKind, forKey: .componentKind)
        try c.encodeIfPresent(componentFramework, forKey: .componentFramework)
    }
}

/// Absolute path of this subproject on disk — EXACTLY the form the build tools
/// receive, and therefore the form under which build status and diagnostics are
/// keyed in the knowledge graph (`build.last.<abs>.<target>`).
///
/// Rules (must match the action runner's own resolution):
///   • already-absolute `path` → used as-is
///   • empty `path`            → the project root (NO trailing slash)
///   • relative `path`         → `<projectRoot>/<path>` (no double slash)
///
/// A divergence here (e.g. a trailing slash when the path is empty) makes the
/// build-status reader miss the key the writer produced, so every target shows
/// "never built" even after a successful build.
extension SubprojectInfo {
    func absolutePath(in projectRoot: String) -> String {
        let root = projectRoot.hasSuffix("/") ? String(projectRoot.dropLast()) : projectRoot
        let p = path.hasSuffix("/") ? String(path.dropLast()) : path
        if p.hasPrefix("/") { return p }
        if p.isEmpty { return root }
        return root + "/" + p
    }
}

/// Editability constraint for LLM modifications inside a domain.
enum DomainEditability: String, Codable {
    case readOnly = "read_only"
    case shared
    case fillable
}

/// A named slice of the project the UI lets the user select and the LLM edits
/// within (e.g. common / rpi5 / rock3c).
struct ProjectDomain: Codable, Identifiable, Hashable {
    var id: String { "domain-\(kind)-\(name)" }
    let name: String
    let kind: String              // "common" | "platform"
    let files: [String]
    let dependencies: [Dependency]
    let editability: DomainEditability
    let contracts: [String]

    enum CodingKeys: String, CodingKey {
        case name, kind, files, dependencies = "dependencies",
             editability = "editability", contracts = "contracts"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "platform"
        files = try c.decodeIfPresent([String].self, forKey: .files) ?? []
        dependencies = try c.decodeIfPresent([Dependency].self, forKey: .dependencies) ?? []
        editability = try c.decodeIfPresent(DomainEditability.self, forKey: .editability) ?? .fillable
        contracts = try c.decodeIfPresent([String].self, forKey: .contracts) ?? []
    }
}

/// A project dependency with name and version requirement.
struct Dependency: Codable, Identifiable, Hashable {
    var id: String { name }
    let name: String
    let version: String?
}

enum SubprojectKind: String, Codable {
    case project   // the main project (root config: Cargo.toml at root)
    case library   // lib
    case binary    // bin
    case cdylib    // dynamic library
    case framework // macOS framework
    case directory // non-subproject top-level directory
    /// A **component** of an ESP-IDF component library: a directory under `components/`. It is the
    /// library's product, and the one thing in the tree a user adds, edits and removes.
    ///
    /// Declared here rather than left to fall back to `unknown`, because a `String`-backed enum
    /// *throws* on a raw value it does not know — one unrecognized kind would take the whole
    /// `ProjectInfo` decode with it, and the project would fail to open rather than show a row.
    case component
    case unknown
}

struct FileEntry: Codable, Identifiable {
    var id: String { path }
    let path: String
    let role: String           // "entry point", "actor", "model", "protocol"
    let sizeBytes: Int
    let language: String
}

struct BuildStatus: Codable {
    let lastBuild: Date?
    let success: Bool?
    let output: String?
    let errors: [String]
    /// Seconds the last build took (nil for older records / unknown).
    let durationSecs: Double?
}

/// A single build/lint diagnostic from the knowledge graph (Diagnostic node).
struct DiagnosticEntry: Codable, Identifiable, Hashable {
    var id: String { "\(file ?? ""):\(line.map(String.init) ?? ""):\(severity)" }
    /// "error", "warning", or "info"
    var severity: String
    /// Absolute or relative file path (nil for project-level diagnostics).
    var file: String?
    var line: Int?
    var column: Int?
    var message: String
    /// "build", "lint", or "fix"
    var buildType: String?
    var buildRunId: String?

    enum CodingKeys: String, CodingKey {
        case severity, file, line, column, message
        case buildType = "buildType"
        case buildRunId = "buildRunId"
    }
}

struct DependencyInfo: Codable, Identifiable {
    var id: String { name }
    let name: String
    let version: String
    let isExternal: Bool       // false = internal workspace dep
}