import Foundation

/// A previously opened project, persisted to ~/.spire/recent-projects.json.
struct RecentProject: Codable, Identifiable, Equatable {
    var id: String { path }
    var path: String
    var name: String
    var lastOpened: Date

    /// Path to the global recent-projects file (~/.spire/recent-projects.json).
    static func storageURL() -> URL {
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home.appendingPathComponent(".spire").appendingPathComponent("recent-projects.json")
    }

    /// Load the list of recent projects (most recent first).
    static func load() -> [RecentProject] {
        let url = storageURL()
        guard let data = try? Data(contentsOf: url) else { return [] }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let loaded = (try? decoder.decode([RecentProject].self, from: data)) ?? []
        return Array(loaded.prefix(5))
    }

    /// Save the list of recent projects (most recent first).
    static func save(_ projects: [RecentProject]) {
        let url = storageURL()
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        if let data = try? encoder.encode(projects) {
            try? data.write(to: url, options: .atomic)
        }
    }

    /// Record a project open: prepend and keep the last 5 unique entries.
    static func record(path: String, name: String) {
        var list = load()
        list.removeAll { $0.path == path }
        list.insert(RecentProject(path: path, name: name, lastOpened: Date()), at: 0)
        if list.count > 5 {
            list = Array(list.prefix(5))
        }
        save(list)
    }

    /// Remove a project from the recent list by path and persist.
    static func remove(path: String) {
        var list = load()
        list.removeAll { $0.path == path }
        save(list)
    }
}
