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
    var id: String { name }
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
    /// Structural shape: "native" | "single_source" | "hal". Empty when the
    /// analyzer didn't classify it (treat as native).
    let structure: String
    /// Named domains (common / rpi5 / rock3c). Empty when the shape is native.
    let domains: [ProjectDomain]

    enum CodingKeys: String, CodingKey {
        case name, kind, buildSystem = "buildSystem",
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
    }

    init(name: String, kind: SubprojectKind, buildSystem: String, path: String,
         language: String, description: String = "", files: [FileEntry]? = nil,
         dependencies: [Dependency]? = nil, buildStatus: BuildStatus? = nil,
         platformTargets: [String] = [], buildTargets: [BuildTarget] = [],
         structure: String = "native", domains: [ProjectDomain] = []) {
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