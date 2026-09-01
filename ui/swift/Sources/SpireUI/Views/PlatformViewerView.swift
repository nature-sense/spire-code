import SwiftUI
import AppKit

enum PlatformPortal {
    private static var windows: [NSWindow] = []
    @MainActor static func open(bridge: SpireBridge, theme: AppTheme) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 560),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Platforms"; w.isReleasedWhenClosed = false
        // Floating windows don't inherit the main window's SwiftUI environment.
        let view = PlatformViewerView().environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        // Enforce size AFTER assigning the hosting controller (AppKit
        // shrinks to the controller's preferred size otherwise).
        w.setContentSize(NSSize(width: 900, height: 560))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 450, y: m.midY - 280)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

struct PlatformViewerView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var platforms: [Platform] = []
    @State private var selected: String?

    var body: some View {
        HStack(spacing: 0) {
            List(platforms) { p in
                Button { selected = p.id } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(p.name).font(.callout.weight(.medium))
                            .foregroundStyle(selected == p.id ? theme.accent : theme.textPrimary)
                        Text("\(p.id) · \(p.os)").font(.caption2).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .listRowBackground(selected == p.id ? theme.accentBackground : Color.clear)
            }
            .frame(width: 220).scrollContentBackground(.hidden).background(theme.surface)
            Divider()
            if let p = platforms.first(where: { $0.id == selected }) { detail(p) }
            else if loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ContentUnavailableView("Select a platform", systemImage: "shippingbox",
                    description: Text("No platforms loaded")).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task { loading = true; platforms = await bridge.fetchPlatforms(); if selected == nil { selected = platforms.first?.id }; loading = false }
    }

    @State private var loading = true

    /// Mirrors `Platform::sysroot_ok()` (spire-modules): a cross target needs a
    /// populated sysroot (`usr/` present); host/native platforms with an empty
    /// root are fine.
    private func sysrootStatus(_ p: Platform) -> (ok: Bool, reason: String?) {
        let root = p.sysroot.root.trimmingCharacters(in: .whitespaces)
        guard !root.isEmpty else { return (true, nil) }   // host / native
        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: root, isDirectory: &isDir), isDir.boolValue else {
            return (false, "Sysroot missing — directory does not exist:\n\(root)")
        }
        var usrDir: ObjCBool = false
        let usr = (root as NSString).appendingPathComponent("usr")
        guard FileManager.default.fileExists(atPath: usr, isDirectory: &usrDir), usrDir.boolValue else {
            return (false, "Sysroot not populated — missing \(usr)")
        }
        return (true, nil)
    }

    private func detail(_ p: Platform) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(p.name).font(.title2.weight(.bold))
                    }
                    Spacer()
                    Button { Task { _ = p.id; loading = true; platforms = await bridge.fetchPlatforms(); loading = false } } label: {
                        Image(systemName: "arrow.clockwise").foregroundStyle(theme.accent)
                    }.buttonStyle(.plain).help("Reload")
                }
                group("Architecture") {
                    row("CPU family", p.architecture.cpuFamily)
                    row("CPU", p.architecture.cpu)
                    row("Endian", p.architecture.endian)
                    row("Triple", p.architecture.targetTriple)
                    if let m = p.architecture.march { row("March", m) }
                }
                group("Toolchain") {
                    row("C", p.toolchain.c); row("C++", p.toolchain.cpp)
                    row("ar", p.toolchain.ar); row("strip", p.toolchain.strip)
                    if let ld = p.toolchain.ld { row("Linker", ld) }
                    if let pg = p.toolchain.pkgconfig { row("pkgconfig", pg) }
                    list("C args", p.toolchain.cArgsExtra)
                    list("C++ args", p.toolchain.cppArgsExtra)
                    list("Linker args", p.toolchain.linkerArgsExtra)
                }
                group("Sysroot") {
                    let status = sysrootStatus(p)
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: status.ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                            .foregroundStyle(status.ok ? Color.green : Color.red)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(status.ok ? "Sysroot OK" : "Sysroot missing")
                                .font(.callout.weight(.semibold))
                                .foregroundStyle(status.ok ? Color.green : Color.red)
                            if let reason = status.reason {
                                Text(reason)
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.red)
                                    .textSelection(.enabled)
                            }
                        }
                        Spacer()
                    }
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        RoundedRectangle(cornerRadius: 6)
                            .fill(status.ok ? Color.green.opacity(0.08) : Color.red.opacity(0.10))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 6)
                            .stroke(status.ok ? Color.green.opacity(0.4) : Color.red.opacity(0.5),
                                    lineWidth: 0.5)
                    )
                    row("Root", p.sysroot.root)
                    list("Lib dirs", p.sysroot.libDirs)
                    list("Include dirs", p.sysroot.includeDirs)
                    list("pkg-config libdir", p.sysroot.pkgConfigLibdir)
                }
            }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func group(_ t: String, @ViewBuilder _ c: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(t.uppercased()).font(.caption.weight(.bold)).foregroundStyle(theme.accent)
            c()
        }
    }
    private func row(_ k: String, _ v: String) -> some View {
        HStack {
            Text(k).font(.callout).foregroundStyle(.secondary).frame(width: 120, alignment: .leading)
            Text(v).font(.callout.monospaced()).textSelection(.enabled)
            Spacer()
        }
    }
    private func list(_ k: String, _ items: [String]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(k).font(.callout).foregroundStyle(.secondary)
            ForEach(items, id: \.self) { Text($0).font(.callout.monospaced()).textSelection(.enabled) }
        }
    }
}