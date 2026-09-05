import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// Opens the AppSpec design session as a large, resizable floating window.
/// Mirrors the RagPortal / TerminalPortal pattern.
enum SpecDesignPortal {
    private static var windows: [NSWindow] = []
    @MainActor static func open(bridge: SpireBridge, theme: AppTheme, projectName: String, goal: String = "") {
        let size = NSSize(width: 1240, height: 820)
        let w = NSWindow(contentRect: NSRect(origin: .zero, size: size),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Design AppSpec — \(projectName)"
        w.isReleasedWhenClosed = false
        let view = SpecDesignView(
            projectName: projectName,
            goal: goal,
            onClose: { [weak w] in w?.close() },
            onDecided: { spec in
                Task { await bridge.runSpecDesignCodegen(spec: spec) }
            }
        )
        .environment(bridge)
        .environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(size)
        if let m = NSApp.mainWindow?.frame {
            w.setFrameOrigin(NSPoint(x: m.midX - size.width / 2, y: m.midY - size.height / 2))
        } else {
            w.center()
        }
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// The AppSpec design step: paste a freeform but detailed spec (or load it from
/// a file), convert it into the strict textual AppSpec, review the generated
/// AppSpec, and accept it once it looks right.
struct SpecDesignView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    let projectName: String
    let goal: String
    /// Called when the session window should close (used by the portal).
    var onClose: () -> Void = {}
    /// Called with the accepted AppSpec dictionary (feeds the wizard's codegen).
    let onDecided: ([String: Any]) -> Void

    @State private var freeform = ""
    @State private var specMd: String?
    @State private var issues: [SpecIssue] = []
    @State private var isDecided = false
    @State private var acceptedCount = 0
    @State private var busy = false
    @State private var errorMessage: String?

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HSplitView {
                freeformPane
                generatedPane
            }
        }
        .frame(minWidth: 1000, minHeight: 700)
        .task {
            let (state, error) = await bridge.specDesignStart(projectName: projectName, goal: goal)
            apply(state: state, error: error)
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: 12) {
            Label("Design AppSpec", systemImage: "doc.text.magnifyingglass")
                .font(.title3.weight(.semibold))
            Text(projectName)
                .font(.callout)
                .foregroundStyle(.secondary)
            Spacer()
            if isDecided {
                Label("AppSpec accepted", systemImage: "checkmark.seal.fill")
                    .font(.caption)
                    .foregroundStyle(.green)
                Text("v\(acceptedCount)").font(.caption).foregroundStyle(.secondary)
                Button("Revise") {
                    Task { await reopen() }
                }
                .buttonStyle(.bordered)
                .disabled(busy)
                .help("Back to editing: change the freeform spec and convert again")
                Button("Done") {
                    onClose()
                }
                .buttonStyle(.borderedProminent)
            } else {
                Text("Freeform spec → AppSpec → Accept")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(12)
    }

    // MARK: - Left: freeform spec

    private var freeformPane: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Label("1. Freeform spec", systemImage: "doc.text")
                    .font(.headline)
                Spacer()
                Button("Load from file…") {
                    loadFreeformFromFile()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(busy || isDecided)
                Button {
                    Task { await convert() }
                } label: {
                    if busy {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("Convert", systemImage: "arrow.triangle.2.circlepath")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
                .disabled(freeform.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || busy || isDecided)
                .help("Convert this freeform spec into the textual AppSpec")
            }
            .padding(10)

            TextEditor(text: $freeform)
                .font(.system(.body, design: .monospaced))
                .scrollContentBackground(.hidden)
                .textSelection(.enabled)
                .padding(6)
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.nodeBackground))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 0.5))
                .overlay(alignment: .topLeading) {
                    if freeform.isEmpty {
                        Text("Paste or write the freeform spec here…\n\nExample:\nA map UI to view GIS features that live in the memory graph. Users can list, select and edit feature attributes; changes are saved back to the feature store.")
                            .font(.callout)
                            .foregroundStyle(.tertiary)
                            .padding(10)
                            .allowsHitTesting(false)
                    }
                }
                .disabled(busy || isDecided)
                .padding([.horizontal, .bottom], 10)
        }
        .frame(minWidth: 380)
    }

    // MARK: - Right: generated AppSpec

    private var generatedPane: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Label("2. Generated AppSpec", systemImage: "doc.richtext")
                    .font(.headline)
                Spacer()
                if let specMd, !specMd.isEmpty {
                    Button { copySpec() } label: {
                        Label("Copy", systemImage: "doc.on.doc").font(.caption.weight(.medium))
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .help("Copy the AppSpec markdown to the clipboard")
                    Button { exportSpec() } label: {
                        Label("Export…", systemImage: "square.and.arrow.up").font(.caption.weight(.medium))
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .help("Save the AppSpec to a .md file")
                }
            }

            issueStrip

            if let specMd, !specMd.isEmpty {
                ScrollView {
                    MarkdownText(specMd)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary.opacity(0.35)))

                HStack {
                    if isDecided {
                        Label("AppSpec accepted — code generation started.", systemImage: "checkmark.circle.fill")
                            .font(.callout)
                            .foregroundStyle(.green)
                    } else {
                        Button {
                            Task { await accept() }
                        } label: {
                            if busy {
                                ProgressView().controlSize(.small)
                            } else {
                                Label("Accept AppSpec", systemImage: "checkmark.seal")
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                        .disabled(busy || hasErrors || specMd.isEmpty)
                        .help(hasErrors
                            ? "Resolve the validation errors in the freeform spec, then Convert again"
                            : "Accept this AppSpec and start code generation")
                        Text(hasErrors ? "Fix the errors below, then Convert again." : "Review, then accept to persist and generate.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                }
                .padding(.top, 4)
            } else {
                VStack(spacing: 10) {
                    Image(systemName: "doc.richtext")
                        .font(.system(size: 36))
                        .foregroundStyle(.tertiary)
                    Text("No AppSpec yet")
                        .font(.headline)
                        .foregroundStyle(.secondary)
                    Text("Write or paste a freeform spec on the left, then press Convert.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding(12)
        .frame(minWidth: 400, maxWidth: .infinity)
    }

    @ViewBuilder
    private var issueStrip: some View {
        if !issues.isEmpty {
            VStack(alignment: .leading, spacing: 3) {
                ForEach(issues, id: \.message) { issue in
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: issue.isError ? "xmark.octagon.fill" : "exclamationmark.triangle.fill")
                            .foregroundStyle(issue.isError ? .red : .orange)
                        Text("\(issue.message)")
                            .font(.caption)
                        Spacer()
                    }
                }
            }
            .padding(6)
            .background(RoundedRectangle(cornerRadius: 6).fill(.orange.opacity(0.08)))
        }
        if let errorMessage {
            Text(errorMessage)
                .font(.caption)
                .foregroundStyle(.red)
        }
    }

    private var hasErrors: Bool {
        issues.contains { $0.isError }
    }

    // MARK: - Actions

    @MainActor
    private func apply(state: SpecDesignState?, error: String?) {
        if let state {
            freeform = state.freeformSpec
            specMd = state.specMd
            issues = state.issues
            isDecided = state.isDecided
            acceptedCount = state.acceptedCount
        }
        if let error {
            errorMessage = error
        }
    }

    @MainActor
    private func convert() async {
        let text = freeform.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (outcome, error) = await bridge.specDesignConvert(projectName: projectName, specText: text)
        if let outcome {
            specMd = outcome.specMd
            issues = outcome.issues
            if let parseError = outcome.parseError {
                errorMessage = "The generated AppSpec could not be parsed: \(parseError)"
            }
            apply(state: outcome.state, error: nil)
        }
        if let error {
            errorMessage = error
        }
    }

    @MainActor
    private func accept() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (spec, error) = await bridge.specDesignAccept(projectName: projectName)
        if let spec {
            onDecided(spec)
            onClose()
        } else if let error {
            errorMessage = error
        } else {
            errorMessage = "Accept returned no AppSpec"
        }
    }

    @MainActor
    private func reopen() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (state, error) = await bridge.specDesignReopen(projectName: projectName)
        apply(state: state, error: error)
    }

    // MARK: - File / clipboard helpers

    private func loadFreeformFromFile() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Load"
        panel.message = "Choose a text file with the freeform spec"
        panel.allowedContentTypes = [.plainText, .text, UTType(filenameExtension: "md") ?? .plainText]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            freeform = try String(contentsOf: url, encoding: .utf8)
        } catch {
            errorMessage = "Load failed: \(error.localizedDescription)"
        }
    }

    private func copySpec() {
        guard let specMd else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(specMd, forType: .string)
    }

    private func exportSpec() {
        guard let specMd else { return }
        let panel = NSSavePanel()
        panel.title = "Export AppSpec"
        panel.nameFieldStringValue = "\(projectName)-appspec.md"
        panel.canCreateDirectories = true
        panel.allowedContentTypes = [UTType(filenameExtension: "md") ?? .plainText]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            try specMd.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            errorMessage = "Export failed: \(error.localizedDescription)"
        }
    }
}

#Preview {
    SpecDesignView(projectName: "spire-gis", goal: "view and edit map layers", onDecided: { _ in })
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .frame(width: 1100, height: 700)
}
