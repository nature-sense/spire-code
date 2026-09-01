import Foundation

/// A file emitted by the scaffold (build config or source stub) for the UI's
/// structure preview. Structural files are locked — the LLM may not modify them.
struct ScaffoldFile: Codable, Identifiable, Hashable {
    let path: String
    let structural: Bool
    let content: String

    var id: String { path }
}

/// The structural contract returned by `createProject/Scaffold`. Tells the
/// fill step (and the UI) which paths are locked, which source roots are
/// writable, which build-config dependency sections are tool-only, and the
/// emitted file list for the structure preview.
///
/// Rust serializes this with snake_case keys; the CodingKeys map them to the
/// camelCase properties used by the UI. `structure`/`embedded`/`fillRole` are
/// newer fields whose presence is optional for backward compatibility.
struct ScaffoldSpec: Codable {
    let structuralFiles: [String]
    let fillRoots: [String]
    let dependencySections: [String]
    let platformTargets: [String]
    let buildSystem: String
    let files: [ScaffoldFile]
    /// "native" | "single_source" | "hal" — the structural shape the scaffold
    /// was emitted for (the embedded wizard's Meson choice). Defaults to
    /// "native" (legacy flat scaffold).
    let structure: String
    /// True when the project is an embedded cross-compile (targets only, no
    /// host build). Defaults to false (host/native project).
    let embedded: Bool

    enum CodingKeys: String, CodingKey {
        case structuralFiles = "structural_files"
        case fillRoots = "fill_roots"
        case dependencySections = "dependency_sections"
        case platformTargets = "platform_targets"
        case buildSystem = "build_system"
        case files
        case structure
        case embedded
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        structuralFiles = try c.decode([String].self, forKey: .structuralFiles)
        fillRoots = try c.decode([String].self, forKey: .fillRoots)
        dependencySections = try c.decode([String].self, forKey: .dependencySections)
        platformTargets = try c.decode([String].self, forKey: .platformTargets)
        buildSystem = try c.decode(String.self, forKey: .buildSystem)
        files = try c.decodeIfPresent([ScaffoldFile].self, forKey: .files) ?? []
        structure = try c.decodeIfPresent(String.self, forKey: .structure) ?? "native"
        embedded = try c.decodeIfPresent(Bool.self, forKey: .embedded) ?? false
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(structuralFiles, forKey: .structuralFiles)
        try c.encode(fillRoots, forKey: .fillRoots)
        try c.encode(dependencySections, forKey: .dependencySections)
        try c.encode(platformTargets, forKey: .platformTargets)
        try c.encode(buildSystem, forKey: .buildSystem)
        try c.encode(files, forKey: .files)
        try c.encode(structure, forKey: .structure)
        try c.encode(embedded, forKey: .embedded)
    }
}
