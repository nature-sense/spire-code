import SwiftUI
import AppKit

/// Opens the HAL documentation viewer (contracts + datatypes + per-platform status).
enum HALViewerPortal {
    private static var windows: [NSWindow] = []

    @MainActor static func open(bridge: SpireBridge, theme: AppTheme, projectRoot: String) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 960, height: 620),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "HAL Documentation"; w.isReleasedWhenClosed = false
        let view = HALViewerView(projectRoot: projectRoot).environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(NSSize(width: 960, height: 620))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 480, y: m.midY - 310)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// The documentation dialog: contract list (left) + rich detail page (right).
struct HALViewerView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let projectRoot: String

    @State private var report: HalDocReport?
    @State private var selectedStem: String?
    @State private var loading = true

    var body: some View {
        HStack(spacing: 0) {
            List(selection: $selectedStem) {
                if let report {
                    ForEach(report.contracts) { c in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(c.stem).font(.callout.weight(.semibold))
                            Text(c.brief).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        }
                        .tag(c.stem)
                    }
                    Section("Core Datatypes") {
                        ForEach(report.types) { t in
                            VStack(alignment: .leading, spacing: 0) {
                                Text(t.name).font(.callout.monospaced())
                                if !t.brief.isEmpty {
                                    Text(t.brief).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                                }
                            }
                            .tag("type:\(t.name)")
                        }
                    }
                }
            }
            .frame(width: 280)

            Divider()

            ScrollView {
                if loading {
                    ProgressView("Loading HAL documentation…").padding()
                } else if let report {
                    if let stem = selectedStem, stem.hasPrefix("type:"), let t = report.types.first(where: { "type:\($0.name)" == stem }) {
                        typePage(t)
                    } else if let stem = selectedStem, let c = report.contracts.first(where: { $0.stem == stem }) {
                        contractPage(c)
                    } else if let first = report.contracts.first {
                        contractPage(first)
                    } else {
                        Text("No HAL contracts found.").foregroundStyle(.secondary).padding()
                    }
                } else {
                    Text("Failed to load HAL documentation.").foregroundStyle(.red).padding()
                }
            }
            .padding()
        }
        .task {
            report = await bridge.halDocs(root: projectRoot)
            selectedStem = report?.contracts.first?.stem
            loading = false
        }
    }

    // MARK: - Contract page (colour-coded tags + prose)

    private func contractPage(_ c: HalContractDoc) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(c.className).font(.title2.bold())
            if !c.contractId.isEmpty { Label(c.contractId, systemImage: "number").font(.caption).foregroundStyle(.secondary) }
            if !c.brief.isEmpty { Text(c.brief).font(.subheadline) }
            if !c.tags.isEmpty { tagFlow(c.tags, prose: c.prose) }
            if !c.prose.isEmpty && c.tags.isEmpty { Text(c.prose).font(.callout).foregroundStyle(.secondary) }
            Text(URL(fileURLWithPath: c.header).lastPathComponent)
                .font(.caption2.monospaced())
                .foregroundStyle(.tertiary)

            if !c.usesTypes.isEmpty {
                Text("Uses: \(c.usesTypes.joined(separator: ", "))").font(.caption).foregroundStyle(.secondary)
            }

            Divider()
            Text("Methods").font(.headline)
            ForEach(c.methods) { m in
                VStack(alignment: .leading, spacing: 3) {
                    Text(signature(m)).font(.callout.monospaced())
                    if !m.prose.isEmpty { Text(m.prose).font(.caption).foregroundStyle(.secondary) }
                    if !m.tags.isEmpty { tagFlow(m.tags, prose: "") }
                }
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            Divider()
            Text("Platform implementations").font(.headline)
            ForEach(c.platforms) { p in
                HStack {
                    Text(p.platform).font(.callout.weight(.semibold))
                    Spacer()
                    if p.implemented {
                        Text("implemented").foregroundStyle(.green).font(.caption.weight(.semibold))
                    } else {
                        Text("missing \(p.missing.count) · drifted \(p.drifted.count)").foregroundStyle(.orange).font(.caption.weight(.semibold))
                    }
                }
                .padding(.vertical, 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Type page (tags + prose + field-level docs)

    private func typePage(_ t: HalTypeDoc) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(t.name).font(.title2.bold().monospaced())
            if !t.brief.isEmpty { Text(t.brief).font(.subheadline) }
            if !t.tags.isEmpty { tagFlow(t.tags, prose: t.prose) }
            if !t.prose.isEmpty && t.tags.isEmpty { Text(t.prose).font(.callout).foregroundStyle(.secondary) }
            Text(URL(fileURLWithPath: t.header).lastPathComponent)
                .font(.caption2.monospaced())
                .foregroundStyle(.tertiary)

            if !t.fields.isEmpty {
                Divider()
                Text("Fields").font(.headline)
                ForEach(t.fields) { f in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(fieldLabel(f))
                        .font(.callout.monospaced().weight(.semibold))
                        .foregroundStyle(.primary)
                        if !f.prose.isEmpty { Text(f.prose).font(.caption).foregroundStyle(.secondary) }
                        if !f.tags.isEmpty { tagFlow(f.tags, prose: "") }
                    }
                    .padding(.vertical, 4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// "uint32_t width" (type + name) — fixes fields showing only the value.
    private func fieldLabel(_ f: HalFieldDoc) -> String {
        if f.typeName.isEmpty { return f.name }
        return "\(f.typeName) \(f.name)"
    }

    private func signature(_ m: HalMethodDoc) -> String {
        let ret = m.returnType.isEmpty ? "" : "\(m.returnType) "
        return "\(ret)\(m.name)(\(m.params))"
    }

    // MARK: - Colour-coded tag badges

    /// Displays every tag as a coloured capsule with its value alongside.
    private func tagFlow(_ tags: [HalDocTag], prose: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 4) {
                    ForEach(Array(tags.enumerated()), id: \.element.name) { _, t in
                        HStack(spacing: 3) {
                            Text(t.name)
                                .font(.caption2.weight(.bold))
                                .foregroundStyle(tagColor(t.name))
                                .padding(.horizontal, 5).padding(.vertical, 2)
                                .background(tagColor(t.name).opacity(0.12), in: Capsule())
                            if !t.value.isEmpty {
                                Text(t.value)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            if !prose.isEmpty {
                Text(prose).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    /// Tag → colour map (custom colour-coded scheme).
    private func tagColor(_ tagName: String) -> Color {
        let t = tagName.lowercased()
        switch t {
        case "@brief", "@id": return .gray
        case "@param", "@return": return .blue
        case "@lifespan", "@ownership", "@zero-copy": return .orange
        case "@platform-note": return .purple
        case "@thread-safety": return .teal
        case "@performance": return .green
        case "@error": return .red
        default: return .gray
        }
    }
}

/// Minimal wrapping layout for rows of small views (tag badges).
struct FlowLayout: Layout {
    let spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .greatestFiniteMagnitude
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0
        for sub in subviews {
            let s = sub.sizeThatFits(.unspecified)
            if x + s.width > maxWidth, x > 0 {
                x = 0; y += rowHeight + spacing; rowHeight = 0
            }
            x += s.width + spacing
            rowHeight = max(rowHeight, s.height)
        }
        return CGSize(width: min(maxWidth, x), height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, rowHeight: CGFloat = 0
        for sub in subviews {
            let s = sub.sizeThatFits(.unspecified)
            if x + s.width > bounds.maxX, x > bounds.minX {
                x = bounds.minX; y += rowHeight + spacing; rowHeight = 0
            }
            sub.place(at: CGPoint(x: x, y: y), proposal: .unspecified)
            x += s.width + spacing
            rowHeight = max(rowHeight, s.height)
        }
    }
}