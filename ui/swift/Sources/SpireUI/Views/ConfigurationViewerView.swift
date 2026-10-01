import SwiftUI
import AppKit

/// Opens the configuration viewer as a floating window (same pattern as the other portals).
enum ConfigurationPortal {
    private static var windows: [NSWindow] = []

    @MainActor static func open(bridge: SpireBridge, theme: AppTheme) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1040, height: 660),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Configuration"
        w.isReleasedWhenClosed = false
        // Floating windows don't inherit the main window's SwiftUI environment.
        let view = ConfigurationViewerView().environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        // Enforce size AFTER assigning the hosting controller (AppKit shrinks to the controller's
        // preferred size otherwise).
        w.setContentSize(NSSize(width: 1040, height: 660))
        if let m = NSApp.mainWindow?.frame {
            w.setFrameOrigin(NSPoint(x: m.midX - 520, y: m.midY - 330))
        } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// The **configuration** screen: every board and chip the graph holds, and every fact declared about
/// it — identity, wiring, capabilities, companions, build facts, device, hints.
///
/// The source is the **graph** (`platforms/config`), not the registry YAML. The files are only the
/// seed; the graph is what a build resolves against, and a screen reading the seed could disagree
/// with it.
struct ConfigurationViewerView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var platforms: [Platform] = []
    @State private var selected: String?
    @State private var search = ""
    @State private var loading = true

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            Divider()
            detailPane
        }
        .task { await reload() }
    }

    private func reload() async {
        loading = true
        platforms = await bridge.fetchPlatformConfigs()
        if selected == nil { selected = platforms.first?.id }
        loading = false
    }

    // MARK: - Master

    private var sidebar: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").font(.caption).foregroundStyle(.secondary)
                TextField("Filter", text: $search).textFieldStyle(.plain).font(.callout)
                if !search.isEmpty {
                    Button { search = "" } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain).foregroundStyle(.secondary)
                }
            }
            .padding(8)
            Divider()
            List {
                ForEach(groups, id: \.kind) { group in
                    Section(group.title) {
                        ForEach(group.items) { platformRow($0) }
                    }
                }
            }
            .scrollContentBackground(.hidden).background(theme.surface)
        }
        .frame(width: 250)
    }

    /// Boards, then chips. The registry holds two kinds of entry and they are *not* interchangeable
    /// — a board names the silicon it carries, a chip names the processor — so they are sectioned
    /// rather than shown as one list. `kind` is **sent by the core**; the fallback covers only a
    /// payload from an older core, which carried `embedded` alone.
    private var groups: [(kind: String, title: String, items: [Platform])] {
        let matches = platforms.filter { p in
            search.isEmpty
                || p.name.localizedCaseInsensitiveContains(search)
                || p.id.localizedCaseInsensitiveContains(search)
                || (p.chip?.localizedCaseInsensitiveContains(search) ?? false)
        }
        return [("board", "Boards"), ("chip", "Chips")]
            .compactMap { kind, title in
                let items = matches.filter { ($0.kind ?? ($0.embedded ? "chip" : "board")) == kind }
                return items.isEmpty ? nil : (kind, title, items)
            }
    }

    private func platformRow(_ p: Platform) -> some View {
        Button { selected = p.id } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(p.name).font(.callout.weight(.medium))
                    .foregroundStyle(selected == p.id ? theme.accent : theme.textPrimary)
                Text("\(p.id) · \(p.os)" + (p.chip.map { " · chip \($0)" } ?? ""))
                    .font(.caption2).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .listRowBackground(selected == p.id ? theme.accentBackground : Color.clear)
    }

    // MARK: - Detail

    @ViewBuilder private var detailPane: some View {
        if let p = platforms.first(where: { $0.id == selected }) {
            detail(p)
        } else if loading {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ContentUnavailableView("Select a board or chip", systemImage: "cpu",
                description: Text("No configuration in the graph"))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func detail(_ p: Platform) -> some View {
        // A board states none of the build facts itself: it names its chip, and the facts are the
        // chip's. So they are read from the chip entry already in this list — the alternative, three
        // empty cards, is what every board looked like before it could say what it carries.
        let facts = p.chip.flatMap { id in platforms.first { $0.id == id } } ?? p
        let blocks = p.capabilityBlocks
        return ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                header(p)
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 300, maximum: 380), spacing: 12)],
                    alignment: .leading,
                    spacing: 12
                ) {
                    card("Identity", "cpu") {
                        row("Id", p.id)
                        row("OS", p.os)
                        if let family = p.family { row("Family", family) }
                        if let chip = p.chip { row("Chip", chip) }
                    }
                    if let blocks, !blocks.isEmpty {
                        if !blocks.provides.isEmpty {
                            card("Provides", "arrow.up.right.circle") {
                                ForEach(blocks.provides) { valueRow($0.capability, $0.properties) }
                            }
                        }
                        if !blocks.realizes.isEmpty {
                            card("Realizes", "checkmark.circle") {
                                ForEach(blocks.realizes) { valueRow($0.capability, $0.properties) }
                            }
                        }
                        if !blocks.pins.isEmpty {
                            card("Pins", "cable.connector") {
                                ForEach(blocks.pins) { valueRow($0.function, $0.properties) }
                            }
                        }
                        if !blocks.carries.isEmpty {
                            card("Carries", "link") {
                                ForEach(blocks.carries) { valueRow($0.chip, $0.properties) }
                            }
                        }
                    } else {
                        card("Capabilities", "square.stack.3d.up") {
                            Text(blocks == nil
                                 ? "Not in the graph yet — restart the app to seed it."
                                 : "None declared.")
                                .font(.callout).foregroundStyle(.secondary)
                        }
                    }
                    buildCard(facts, chipId: p.chip == nil ? nil : facts.id)
                    // Two layers, two scopes: `hal` is the **chip's** vendor HAL, so it is read off
                    // `facts` — the chip a board names — while `bsp` is the **board's** own support
                    // crate and is read straight off `p`. A board with neither renders neither.
                    if let hal = facts.hal {
                        crateCard("HAL", "square.stack.3d.down.right", hal.crateName,
                                  version: hal.version, features: hal.features)
                    }
                    // The BSP is the board's, and under ESP-IDF a vendor **component** — a name, not
                    // a crate with a version and features, so it is one row rather than a `crateCard`.
                    if let bsp = p.bsp {
                        card("BSP", "square.stack.3d.up") { row("Component", bsp) }
                    }
                    if let rust = facts.rust {
                        card("Rust toolchain", "gearshape.2") {
                            row("Target", rust.target)
                            if let t = rust.idfTarget { row("IDF target", t) }
                            if let f = rust.flash { row("Flash", f) }
                        }
                    }
                    if let device = p.device {
                        card("Device", "antenna.radiowaves.left.and.right") {
                            if let mcp = device.mcp { row("MCP URL", mcp.url) }
                            if let deploy = device.deploy { row("Deploy to", deploy.dest) }
                        }
                    }
                    if let hints = p.libraryHints, !hints.isEmpty {
                        card("Library hints", "text.book.closed") {
                            Text(hints).font(.callout.monospaced()).textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func header(_ p: Platform) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(p.name).font(.title2.weight(.bold))
                Text("\(p.kind ?? (p.embedded ? "chip" : "board")) · \(p.id)")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button { Task { await reload() } } label: {
                Image(systemName: "arrow.clockwise").foregroundStyle(theme.accent)
            }.buttonStyle(.plain).help("Reload from the graph")
        }
    }

    /// The build facts as one card: which chip's facts these are (a board states none itself), then
    /// the arch, the C toolchain and the sysroot.
    @ViewBuilder private func buildCard(_ facts: Platform, chipId: String?) -> some View {
        card("Build", "hammer") {
            if let chipId {
                sub("Chip") {
                    row("Chip", chipId)
                    row("Name", facts.name)
                }
            }
            sub("Architecture") {
                row("CPU family", facts.architecture.cpuFamily)
                row("CPU", facts.architecture.cpu)
                row("Endian", facts.architecture.endian)
                row("Triple", facts.architecture.targetTriple)
                if let m = facts.architecture.march { row("March", m) }
            }
            sub("Toolchain") {
                row("C", facts.toolchain.c); row("C++", facts.toolchain.cpp)
                row("ar", facts.toolchain.ar); row("strip", facts.toolchain.strip)
                if let ld = facts.toolchain.ld { row("Linker", ld) }
                if let pg = facts.toolchain.pkgconfig { row("pkgconfig", pg) }
                list("C args", facts.toolchain.cArgsExtra)
                list("C++ args", facts.toolchain.cppArgsExtra)
                list("Linker args", facts.toolchain.linkerArgsExtra)
            }
            sub("Sysroot") {
                row("Root", facts.sysroot.root)
                list("Lib dirs", facts.sysroot.libDirs)
                list("Include dirs", facts.sysroot.includeDirs)
                list("pkg-config libdir", facts.sysroot.pkgConfigLibdir)
            }
        }
    }

    /// The chip's vendor HAL crate — `crate`, `version`, `features`. Version and features are
    /// dropped when empty rather than shown as blanks.
    private func crateCard(_ title: String, _ icon: String, _ crateName: String,
                           version: String, features: [String]) -> some View {
        card(title, icon) {
            row("Crate", crateName)
            if !version.isEmpty { row("Version", version) }
            if !features.isEmpty { row("Features", features.joined(separator: ", ")) }
        }
    }

    // MARK: - Row helpers

    /// A card: the app's established surface — rounded, hairline border, a titled header with an
    /// icon — so the detail pane reads as a grid of segments rather than one long column of rows.
    private func card(_ title: String, _ icon: String, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: icon)
                .font(.headline)
                .foregroundStyle(theme.textPrimary)
            content()
        }
        .padding(12)
        // Fill the grid cell — **maxHeight too**. `LazyVGrid` sizes each row to its tallest card, so
        // a card that only takes its natural height leaves dead space below it inside the row and
        // the gap to the next row varies card to card. Filling the cell makes every card in a row
        // the same height, so the only vertical gap is the grid's own fixed `spacing`.
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(theme.surface)
                .shadow(color: .black.opacity(0.15), radius: 2, y: 1)
        )
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
    }

    /// A sub-heading inside a card, for the build facts' own three parts.
    private func sub(_ title: String, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title.uppercased())
                .font(.caption2.weight(.bold))
                .foregroundStyle(theme.textSecondary)
            content()
        }
        // A blank line above each heading so the small-caps (`ARCHITECTURE`, `TOOLCHAIN`, …) don't
        // sit flush against the group above them, which read as compressed.
        .padding(.top, 8)
    }

    private func row(_ key: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(key).font(.callout).foregroundStyle(theme.textSecondary)
                .frame(width: 104, alignment: .leading)
            Text(value).font(.callout.monospaced()).textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }

    private func list(_ key: String, _ items: [String]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(key).font(.callout).foregroundStyle(.secondary)
            ForEach(items, id: \.self) { Text($0).font(.callout.monospaced()).textSelection(.enabled) }
        }
    }

    /// A named thing and its values: the name in monospace, then one row per property. A list or
    /// object value is summarised by `JSONValue.display`, so the row stays a row.
    private func valueRow(_ name: String, _ props: [String: JSONValue]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(name).font(.callout.monospaced()).textSelection(.enabled)
            if props.isEmpty {
                Text("—").font(.caption).foregroundStyle(.tertiary)
            } else {
                ForEach(props.sorted(by: { $0.key < $1.key }), id: \.key) { key, value in
                    HStack(alignment: .top, spacing: 8) {
                        Text(key).font(.caption).foregroundStyle(theme.textSecondary)
                            .frame(width: 96, alignment: .leading)
                        Text(value.display).font(.caption.monospaced()).textSelection(.enabled)
                        Spacer(minLength: 0)
                    }
                }
            }
        }
        .padding(.vertical, 2)
    }
}

