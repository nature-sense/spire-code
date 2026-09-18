import SwiftUI

/// Trap control: what the board is **running**, with its output and a stop button.
///
/// The board owns the process table (it is the machine holding the processes), so this view is a
/// reader of one payload — `device/procs` — plus the actions that address a process by name. It
/// deliberately does not cache: a process that exited since the last refresh shows as `alive: false`
/// in the next reply, which is a fact the board knows and this window should not guess.
struct DeviceProcessesSection: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let platform: String
    /// The artifact a "start" would run, when the project has one built for this platform.
    let artifactPath: String?

    @State private var processes: [DeviceProcess] = []
    @State private var note: String?
    @State private var busy = false
    @State private var logFor: String?
    @State private var processLog = ""

    /// One entry, as `device/procs` reports it.
    ///
    /// `alive` is the board's answer (`kill -0`), not an inference from the entry existing — the
    /// difference between "it crashed" and "it never started".
    struct DeviceProcess: Identifiable, Hashable {
        let name: String
        let pid: Int
        let alive: Bool
        let uptimeMs: Int
        let log: String
        var id: String { name }

        /// `name · pid 1234 · up 2m 5s` — or `exited`, which is the part that matters.
        var label: String {
            alive
                ? "\(name) · pid \(pid) · up \(Self.humanUptime(uptimeMs))"
                : "\(name) · pid \(pid) · exited"
        }

        /// Milliseconds as the shortest honest duration: `45s`, `2m 5s`, `1h 3m`.
        static func humanUptime(_ milliseconds: Int) -> String {
            let seconds = max(0, milliseconds / 1000)
            if seconds < 60 { return "\(seconds)s" }
            if seconds < 3600 { return "\(seconds / 60)m \(seconds % 60)s" }
            return "\(seconds / 3600)h \((seconds % 3600) / 60)m"
        }

        /// The `device/procs` payload → entries.
        ///
        /// Pure and static so the decode is pinned against the tool's exact JSON: a field that stopped
        /// decoding would show an empty list, which looks like "nothing is running" rather than like a
        /// bug — the worst way for this particular view to fail.
        static func from(_ reply: [String: Any]?) -> [DeviceProcess] {
            let entries = (reply?["result"] as? [String: Any])?["running"] as? [[String: Any]]
                ?? reply?["running"] as? [[String: Any]]
                ?? []
            return entries.compactMap { entry in
                guard let name = entry["name"] as? String, !name.isEmpty else { return nil }
                return DeviceProcess(
                    name: name,
                    pid: entry["pid"] as? Int ?? 0,
                    alive: entry["alive"] as? Bool ?? false,
                    uptimeMs: entry["uptime_ms"] as? Int ?? 0,
                    log: entry["log"] as? String ?? ""
                )
            }
        }

        /// A tool result's `structured_content`, however the reply nests it.
        static func structured(_ reply: [String: Any]?) -> [String: Any]? {
            (reply?["result"] as? [String: Any]) ?? reply
        }

        /// The tail `device/logs` returns.
        static func logTail(_ reply: [String: Any]?) -> String {
            structured(reply)?["tail"] as? String ?? ""
        }
    }
}

extension DeviceProcessesSection {
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("Board processes").font(.callout.weight(.semibold))
                Button {
                    Task { await refresh() }
                } label: {
                    Label(busy ? "Working…" : "Refresh", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .disabled(busy)
                Spacer()
                if let artifactPath {
                    Button {
                        Task { await start(artifactPath) }
                    } label: {
                        Label("Start binary", systemImage: "play.circle")
                    }
                    .buttonStyle(.borderless)
                    .disabled(busy)
                    .help("Run \(artifactPath) on the board in the background, logging its output")
                }
            }

            if processes.isEmpty {
                Text("Nothing started by Spire is running on \(platform).")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                ForEach(processes) { process in
                    HStack(spacing: 8) {
                        Circle()
                            .fill(process.alive ? Color.green : Color.secondary)
                            .frame(width: 7, height: 7)
                        Text(process.label).font(.caption.monospaced())
                        Spacer(minLength: 0)
                        Button("Logs") { Task { await showLogs(process) } }
                            .buttonStyle(.borderless)
                            .disabled(busy)
                        Button("Stop") { Task { await stop(process) } }
                            .buttonStyle(.borderless)
                            .disabled(busy)
                    }
                    if logFor == process.name {
                        ScrollView {
                            Text(processLog.isEmpty ? "(no output yet)" : processLog)
                                .font(.caption2.monospaced())
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                        }
                        .frame(maxHeight: 140)
                        .padding(6)
                        .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                    }
                }
            }

            if let note {
                Text(note).font(.caption2).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .task { await refresh() }
    }

    @MainActor
    private func refresh() async {
        busy = true
        defer { busy = false }
        let reply = await bridge.deviceProcesses(platform: platform)
        if let error = reply?["error"] as? String {
            note = error
            processes = []
            return
        }
        processes = DeviceProcess.from(reply)
        let alive = processes.filter(\.alive).count
        note = processes.isEmpty ? nil : "\(alive) running of \(processes.count)"
    }

    @MainActor
    private func showLogs(_ process: DeviceProcess) async {
        busy = true
        defer { busy = false }
        let reply = await bridge.deviceLogs(platform: platform, name: process.name)
        if let error = reply?["error"] as? String {
            note = error
            return
        }
        processLog = DeviceProcess.logTail(reply)
        logFor = process.name
    }

    @MainActor
    private func stop(_ process: DeviceProcess) async {
        busy = true
        defer { busy = false }
        let reply = await bridge.deviceStop(platform: platform, name: process.name)
        if let error = reply?["error"] as? String {
            note = error
            return
        }
        let signal = DeviceProcess.structured(reply)?["signal"] as? String
        note = "stopped \(process.name)\(signal.map { " (\($0))" } ?? "")"
        logFor = nil
        await refresh()
    }

    @MainActor
    private func start(_ path: String) async {
        busy = true
        defer { busy = false }
        // The name is the platform: one Spire-started process per board is the honest default, and a
        // second start is refused by the tool rather than silently replacing this one.
        let reply = await bridge.deviceStart(platform: platform, name: platform, path: path)
        if let error = reply?["error"] as? String {
            note = error
            return
        }
        let pid = DeviceProcess.structured(reply)?["pid"] as? Int
        note = "started \(platform)\(pid.map { " (pid \($0))" } ?? "")"
        await refresh()
    }
}

