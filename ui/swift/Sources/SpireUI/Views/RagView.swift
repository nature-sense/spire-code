import SwiftUI
import AppKit

/// Opens the RAG panel as a floating window (same pattern as `PlatformPortal`).
enum RagPortal {
    private static var windows: [NSWindow] = []
    @MainActor static func open(bridge: SpireBridge, theme: AppTheme) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 940, height: 640),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "RAG Knowledge"; w.isReleasedWhenClosed = false
        // Floating windows don't inherit the main window's SwiftUI environment.
        let view = RagView().environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(NSSize(width: 940, height: 640))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 470, y: m.midY - 320)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// The RAG panel: platforms ↔ ingested corpora, ingest manifests ("scripts"),
/// ingest controls, corpus state, and semantic search.
struct RagView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    @State private var platforms: [Platform] = []
    @State private var domains: [RagDomainInfo] = []
    @State private var manifests: [RagManifestInfo] = []
    @State private var selectedPlatformId: String?
    @State private var loading = true

    // Search state
    @State private var searchQuery = ""
    @State private var searchResults: [RagChunkResult] = []
    @State private var searching = false

    // Ingest state
    @State private var ingestingPath: String?
    @State private var lastIngestMessage: String?
    /// Per-domain persisted per-source statuses (fetched from the
    /// KnowledgeStore on load — visible WITHOUT re-ingesting).
    @State private var sourcesByDomain: [String: [RagSourceStatus]] = [:]

    var body: some View {
        HStack(spacing: 0) {
            // ── Left: platform ↔ corpus table ──
            platformList
                .frame(width: 220)

            Divider()

            // ── Center: manifests + ingest control ──
            manifestPane
                .frame(width: 300)

            Divider()

            // ── Right: state + search ──
            detailPane
                .frame(maxWidth: .infinity)
        }
        .task { await load() }
    }

    /// Fresh load of platforms + domain summaries + manifests.
    private func load() async {
        loading = true
        async let ps = bridge.fetchPlatforms()
        async let ds = bridge.fetchRagDomains()
        async let ms = bridge.fetchRagManifests()
        let (p, d, m) = await (ps, ds, ms)
        platforms = p
        domains = d
        manifests = m
        // Persisted per-source status (survives reloads; no ingest required).
        var byDomain: [String: [RagSourceStatus]] = [:]
        for manifest in m {
            let dom = manifest.domain
            if !dom.isEmpty && byDomain[dom] == nil {
                byDomain[dom] = await bridge.fetchRagSources(domain: dom)
            }
        }
        sourcesByDomain = byDomain
        if selectedPlatformId == nil {
            selectedPlatformId = platforms.first?.id
        }
        if let id = selectedPlatformId {
            await bridge.setRagDomain(domain: id)
        }
        loading = false
    }

    // MARK: - Platform ↔ corpus list

    private var platformList: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Platforms")
                .font(.subheadline.weight(.semibold))
                .padding(.horizontal, 10)
                .padding(.top, 10)

            if loading {
                ProgressView().padding()
                Spacer()
            } else if platforms.isEmpty && domains.isEmpty {
                ContentUnavailableView("No knowledge yet", systemImage: "books.vertical",
                    description: Text("Run an ingest manifest to build the corpus."))
                Spacer()
            } else {
                List {
                    // Every registry platform gets a row (even without a corpus).
                    ForEach(platforms) { p in
                        row(platform: p)
                    }
                    // Domains that exist in the store but have no registry
                    // entry (legacy `rag.yaml` without a platform seed).
                    ForEach(orphanDomains) { d in
                        orphanRow(domain: d)
                    }
                }
                .scrollContentBackground(.hidden)
            }
        }
        .background(theme.surface)
    }

    /// Registry platforms joined with their corpus summary (if ingested).
    private func row(platform: Platform) -> some View {
        let domain = domains.first { $0.id == platform.id }
        return Button {
            selectedPlatformId = platform.id
            Task { await bridge.setRagDomain(domain: platform.id) }
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Image(systemName: domain.map { $0.chunkCount > 0 } == true ? "checkmark.circle.fill" : "circle.dashed")
                        .font(.caption)
                        .foregroundStyle(domain.map { $0.chunkCount > 0 } == true ? theme.accent : .secondary)
                    Text(platform.name).font(.callout.weight(.medium))
                        .foregroundStyle(selectedPlatformId == platform.id ? theme.accent : theme.textPrimary)
                }
                Text(summaryLine(domain))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .listRowBackground(selectedPlatformId == platform.id ? theme.accentBackground : Color.clear)
    }

    private func orphanRow(domain: RagDomainInfo) -> some View {
        Button {
            selectedPlatformId = domain.id
            Task { await bridge.setRagDomain(domain: domain.id) }
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Image(systemName: domain.chunkCount > 0 ? "checkmark.circle.fill" : "circle.dashed")
                        .font(.caption)
                        .foregroundStyle(domain.chunkCount > 0 ? theme.accent : .secondary)
                    Text(domain.id).font(.callout.weight(.medium))
                        .foregroundStyle(selectedPlatformId == domain.id ? theme.accent : theme.textPrimary)
                }
                Text(summaryLine(domain))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .listRowBackground(selectedPlatformId == domain.id ? theme.accentBackground : Color.clear)
    }

    private var orphanDomains: [RagDomainInfo] {
        let knownIds = Set(platforms.map(\.id))
        return domains.filter { !knownIds.contains($0.id) }
    }

    private func summaryLine(_ d: RagDomainInfo?) -> String {
        guard let d else { return "No corpus" }
        if d.chunkCount == 0 { return "Not ingested" }
        return "\(d.chunkCount) chunks · \(d.sourceCount) sources · \(d.tokenCount) tokens"
    }

    // MARK: - Ingest manifests (the "scripts")

    private var manifestPane: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Ingest Scripts")
                .font(.subheadline.weight(.semibold))
                .padding(.horizontal, 10)
                .padding(.top, 10)

            Text("ingestion.yaml manifests (per platform)")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10)

            if manifests.isEmpty {
                ContentUnavailableView("No manifests", systemImage: "doc.text.magnifyingglass",
                    description: Text("Place an ingestion.yaml in ~/.spire/knowledge/<platform>/"))
                    .frame(maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(manifests) { m in
                            manifestCard(m)
                        }
                    }
                    .padding(10)
                }
            }

            if let msg = lastIngestMessage {
                Text(msg)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 8)
            }

            Button {
                Task { await load() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
                    .font(.caption.weight(.medium))
            }
            .buttonStyle(.bordered)
            .padding(10)
        }
        .background(theme.surface)
    }

    private func manifestCard(_ m: RagManifestInfo) -> some View {
        let isIngesting = ingestingPath == m.path
        let currentDomain = domains.first { $0.id == m.domain }
        let stale = currentDomain.map { !$0.corpusVersion.isEmpty && $0.corpusVersion != m.corpusVersion } ?? true
        let ingested = currentDomain.map { $0.corpusVersion == m.corpusVersion } ?? false

        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 4) {
                Image(systemName: "doc.badge.gearshape")
                    .foregroundStyle(theme.accent)
                Text(m.platformId)
                    .font(.callout.weight(.semibold))
                if let srcs = sourcesByDomain[m.domain], !srcs.isEmpty {
                    Text("\(srcs.count) sources")
                        .font(.caption2.weight(.medium))
                        .foregroundStyle(theme.accent)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(Capsule().fill(theme.accentBackground))
                }
            }
            Text((m.path as NSString).lastPathComponent)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Text("corpus \(m.corpusVersion.prefix(8))")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .monospaced()

            HStack(spacing: 8) {
                if isIngesting {
                    ProgressView().controlSize(.small)
                    Text("Ingesting…").font(.caption).foregroundStyle(.secondary)
                } else if ingested {
                    Label("Ingested", systemImage: "checkmark.circle.fill")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(theme.accent)
                } else if stale {
                    Label("Stale", systemImage: "exclamationmark.triangle.fill")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.orange)
                }
                Spacer()
                Button {
                    Task { await ingest(m) }
                } label: {
                    Label("Ingest", systemImage: "play.fill")
                        .font(.caption.weight(.medium))
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(isIngesting)
            }
            if let sources = sourcesByDomain[m.domain] {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(sources) { src in
                        HStack(spacing: 4) {
                            Image(systemName: src.status == "ok" ? "checkmark.circle" : "exclamationmark.triangle")
                                .font(.caption2)
                                .foregroundStyle(src.status == "ok" ? theme.accent : .orange)
                            Text(src.id)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Spacer()
                            Text(src.status == "ok" ? "\(src.chunks) chunks" : src.reason)
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                        }
                    }
                }
                .padding(.top, 4)
            }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.nodeBackground))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
    }

    private func ingest(_ m: RagManifestInfo) async {
        ingestingPath = m.path
        lastIngestMessage = nil
        let report = await bridge.ingestRagManifest(path: m.path)
        ingestingPath = nil
        if let report {
            let skipped = report.sources.filter { $0.status != "ok" }
            if skipped.isEmpty {
                lastIngestMessage = "Ingested \(report.chunks) chunks, \(report.entities) entities, \(report.relationships) relationships."
            } else {
                lastIngestMessage = "Ingested \(report.chunks) chunks; \(skipped.count) source(s) skipped."
            }
        } else {
            lastIngestMessage = "Ingest failed (see log)."
        }
        await load()
    }

    // MARK: - State + search

    private var detailPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Corpus State")
                .font(.subheadline.weight(.semibold))

            if let selected = selectedDomain {
                domainDetail(selected)
            } else {
                Text("Select a platform")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Divider()

            Text("Search")
                .font(.subheadline.weight(.semibold))

            HStack(spacing: 6) {
                TextField("Query the corpus…", text: $searchQuery)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { Task { await runSearch() } }
                Button {
                    Task { await runSearch() }
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .buttonStyle(.borderedProminent)
                .disabled(searching || searchQuery.trimmingCharacters(in: .whitespaces).isEmpty)
            }

            if searching {
                ProgressView().frame(maxWidth: .infinity)
            } else if let selected = selectedDomain, searchResults.isEmpty, !searchQuery.isEmpty {
                Text("No results")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                resultsList
            }

            Spacer()
        }
        .padding(12)
    }

    private var selectedDomain: RagDomainInfo? {
        guard let id = selectedPlatformId else { return nil }
        return domains.first { $0.id == id }
    }

    private func domainDetail(_ d: RagDomainInfo) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: d.chunkCount > 0 ? "checkmark.circle.fill" : "circle.dashed")
                    .foregroundStyle(d.chunkCount > 0 ? theme.accent : .secondary)
                Text(d.id).font(.callout.weight(.semibold))
            }
            statsRow("Chunks", "\(d.chunkCount)")
            statsRow("Sources", "\(d.sourceCount)")
            statsRow("Tokens", "\(d.tokenCount)")
            statsRow("Corpus version", d.corpusVersion.isEmpty ? "—" : d.corpusVersion)
            if !d.description.isEmpty {
                Text(d.description).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private func statsRow(_ key: String, _ value: String) -> some View {
        HStack {
            Text(key).font(.caption).foregroundStyle(.secondary)
            Spacer()
            Text(value).font(.caption.monospaced())
        }
    }

    @ViewBuilder
    private var resultsList: some View {
        if searchResults.isEmpty {
            Text("Results appear here.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(searchResults) { r in
                        resultCard(r)
                    }
                }
            }
        }
    }

    private func resultCard(_ r: RagChunkResult) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 4) {
                Text(r.sourcePath).font(.caption.weight(.medium))
                    .foregroundStyle(theme.accent)
                    .lineLimit(1)
                Spacer()
                Text(String(format: "%.2f", r.score))
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
            Text(r.text)
                .font(.caption)
                .foregroundStyle(theme.textPrimary)
                .lineLimit(4)
        }
        .padding(6)
        .background(RoundedRectangle(cornerRadius: 6).fill(theme.nodeBackground))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 0.5))
    }

    private func runSearch() async {
        guard let domainId = selectedPlatformId, !domainId.isEmpty else { return }
        let q = searchQuery.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return }
        searching = true
        searchResults = await bridge.ragSearch(domain: domainId, query: q)
        searching = false
    }
}

#Preview {
    RagView()
        .frame(width: 940, height: 640)
}