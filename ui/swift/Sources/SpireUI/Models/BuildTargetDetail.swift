import Foundation

/// Target-scoped detail returned by `project/getBuildTarget`.
struct BuildTargetDetail: Codable {
    let name: String
    let kind: String
    let configFile: String?
    let platform: [String]
    let dependencies: [BuildTargetDependency]
    let files: [BuildTargetFile]

    enum CodingKeys: String, CodingKey {
        case name, kind, platform, dependencies, files
        case configFile = "configFile"
    }
}

struct BuildTargetDependency: Codable, Identifiable {
    var id: String { name }
    let name: String
    let version: String?

    enum CodingKeys: String, CodingKey {
        case name, version
    }
}

struct BuildTargetFile: Codable, Identifiable {
    var id: String { path ?? "" }
    let path: String?
    let language: String?
    let role: String?
    let lines: Int?
}