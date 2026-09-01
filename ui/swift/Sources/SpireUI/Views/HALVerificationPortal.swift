import SwiftUI
import AppKit

/// Opens the HAL verification dialog (separate from the docs viewer).
enum HALVerificationPortal {
    private static var windows: [NSWindow] = []

    @MainActor static func open(bridge: SpireBridge, theme: AppTheme, projectRoot: String) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 820, height: 560),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Verify HAL"; w.isReleasedWhenClosed = false
        let view = HALVerificationView(projectRoot: projectRoot).environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(NSSize(width: 820, height: 560))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 410, y: m.midY - 280)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// The verification dialog: severity-badged issue list with a summary line.
struct HALVerificationView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let projectRoot: String

    @State private var issues: [HalVerificationIssue]?
    @State private var loading = true

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("HAL Verification").font(.title2.bold())
            if let issues {
                let errors = issues.filter { $0.severity == "error" }.count
                let warnings = issues.filter { $0.severity == "warning" }.count
                Text("\(errors) errors · \(warnings) warnings · \(issues.count - errors - warnings) info")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    if let issues {
                        if issues.isEmpty {
                            Text("No issues — the HAL is healthy. ✓").foregroundStyle(.green).padding()
                        } else {
                            ForEach(issues) { issue in
                                HStack(alignment: .top, spacing: 8) {
                                    badge(issue.severity)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(issue.title).font(.callout.weight(.semibold))
                                        Text(issue.message).font(.caption)
                                        Text(issue.path).font(.caption2.monospaced()).foregroundStyle(.tertiary)
                                        Text("Fix: \(issue.suggestedFix)").font(.caption2).foregroundStyle(.secondary)
                                    }
                                    Spacer(minLength: 0)
                                }
                                .padding(8)
                                .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
                                .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
                            }
                        }
                    } else if loading {
                        ProgressView("Running verification…").padding()
                    } else {
                        Text("Failed to run verification.").foregroundStyle(.red).padding()
                    }
                }
            }
        }
        .padding()
        .task {
            issues = await bridge.halVerify(root: projectRoot)
            loading = false
        }
    }

    private func badge(_ severity: String) -> some View {
        let color: Color = severity == "error" ? .red : (severity == "warning" ? .orange : .gray)
        return Text(severity.uppercased())
            .font(.caption2.weight(.bold))
            .foregroundStyle(color)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(color.opacity(0.12), in: Capsule())
    }
}

/// Opens the HAL Doc Linter dialog (report-only; LLM-friendly fix prompts).
enum HALDocLintPortal {
    private static var windows: [NSWindow] = []

    @MainActor static func open(bridge: SpireBridge, theme: AppTheme, projectRoot: String) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 860, height: 580),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "HAL Doc Linter"; w.isReleasedWhenClosed = false
        let view = HALDocLintView(projectRoot: projectRoot).environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(NSSize(width: 860, height: 580))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 430, y: m.midY - 290)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
    }
}

/// The doc-lint dialog: per-file issues with severity + symbol + LLM fix prompt.
struct HALDocLintView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let projectRoot: String

    @State private var report: HalDocLintReport?
    @State private var loading = true

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("HAL Documentation Linter").font(.title2.bold())
            if let report {
                let total = report.files.map { $0.issues.count }.reduce(0, +)
                Text("\(report.files.count) files · \(total) issues")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if let report {
                        if report.files.allSatisfy({ $0.issues.isEmpty }) {
                            Text("All HAL api headers pass the doc lint. ✓")
                                .foregroundStyle(.green).padding()
                        } else {
                            ForEach(Array(report.files.enumerated()), id: \.element.path) { _, file in
                                VStack(alignment: .leading, spacing: 4) {
                                    HStack(spacing: 6) {
                                        Text(URL(fileURLWithPath: file.path).lastPathComponent)
                                            .font(.callout.weight(.semibold))
                                        SyntaxBadge(path: file.path)
                                    }
                                    ForEach(file.issues) { issue in
                                        HStack(alignment: .top, spacing: 8) {
                                            let color: Color = issue.severity == "error" ? .red : .orange
                                            Text(issue.severity.uppercased())
                                                .font(.caption2.weight(.bold))
                                                .foregroundStyle(color)
                                                .padding(.horizontal, 5).padding(.vertical, 2)
                                                .background(color.opacity(0.12), in: Capsule())
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text("\(issue.symbol) — \(issue.title)")
                                                    .font(.caption.weight(.semibold))
                                                Text(issue.message).font(.caption)
                                                Text("Fix: \(issue.fixPrompt)")
                                                    .font(.caption2).foregroundStyle(.secondary)
                                            }
                                            Spacer(minLength: 0)
                                        }
                                        .padding(6)
                                        .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                                        .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 0.5))
                                    }
                                }
                                .padding(8)
                            }
                        }
                    } else if loading {
                        ProgressView("Running doc linter…").padding()
                    } else {
                        Text("Failed to run doc linter.").foregroundStyle(.red).padding()
                    }
                }
            }
        }
        .padding()
        .task {
            report = await bridge.halDocLint(root: projectRoot)
            loading = false
        }
    }
}

/// Linter-style file viewer for a HAL fill item: shows the generated files
/// (highlighted C++) with Confirm (write just these files) / Reject.
///
/// For a NEW class module (`kind == "none"`) this renders the module PAIR —
/// the concrete declaration `.hpp` and definition `.cpp` as two tabs — because
/// they are one atomic unit. Partial gaps show only the `.gap.cpp`.
struct HALFillFileViewer: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let item: HalFillItem
    /// Called when the user confirms — writes the pair via hal_fill_apply.
    let onConfirm: () -> Void
    /// Called when the user rejects/dismisses.
    var onReject: () -> Void = {}

    /// Selected file tab: 0 = declaration (.hpp), 1 = definition (.cpp).
    @State private var tab: Int = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "doc.text")
                    .foregroundStyle(theme.accent)
                Text(fileTitle)
                    .font(.headline)
                    .lineLimit(1)
                Spacer()
                Text(item.displayKind)
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .background(RoundedRectangle(cornerRadius: 4)
                        .fill(item.kind == "partial" ? Color.orange.opacity(0.2) : Color.blue.opacity(0.2)))
                Button(action: onReject) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Close")
            }

            // Module pair → two tabs (declaration + definition).
            if item.declaration_content != nil && !(item.declaration_content?.isEmpty ?? true) {
                Picker("File", selection: $tab) {
                    Text(".hpp").tag(0)
                    Text(".cpp").tag(1)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 180)
            }

            if let content = visibleContent, !content.isEmpty {
                ScrollView([.horizontal, .vertical]) {
                    Text(SyntaxHighlighter.highlight(content, language: .cpp))
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                }
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.textBackground))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1))
            } else {
                VStack(spacing: 8) {
                    Text("No generated content for this item")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }

            HStack(spacing: 8) {
                Text("Confirm writes \(fileCountText) and re-analyzes the HAL.")
                    .font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Reject", role: .cancel) { onReject() }
                Button("Confirm") { onConfirm() }
                    .buttonStyle(.borderedProminent)
                    .tint(.green)
            }
        }
        .padding(16)
        .frame(width: 720, height: 520)
        .background(theme.background)
    }

    private var visibleContent: String? {
        if tab == 0, let h = item.declaration_content, !h.isEmpty { return h }
        return item.content
    }

    private var fileTitle: String {
        if tab == 0, let p = item.declaration_path, !p.isEmpty {
            return (p as NSString).lastPathComponent
        }
        return (item.create_file as NSString).lastPathComponent
    }

    private var fileCountText: String {
        let hasPair = item.declaration_content != nil && !(item.declaration_content?.isEmpty ?? true)
        return hasPair ? "both files" : "this file"
    }
}

/// Green/red badge showing structural C++ validity for a header.
struct SyntaxBadge: View {
    @Environment(SpireBridge.self) private var bridge
    let path: String

    @State private var report: CppSyntaxReport?
    @State private var loaded = false

    var body: some View {
        Group {
            if let report {
                if report.ok {
                    Text("syntax ✓").font(.caption2.weight(.semibold)).foregroundStyle(.green)
                } else {
                    Text("syntax ✗ \(report.errors.count)").font(.caption2.weight(.semibold)).foregroundStyle(.red)
                }
            } else if loaded {
                Text("syntax ?").font(.caption2.weight(.semibold)).foregroundStyle(.gray)
            } else {
                Text("…").font(.caption2).foregroundStyle(.secondary)
            }
        }
        .task {
            report = await bridge.cppSyntaxCheck(path: path)
            loaded = true
        }
    }
}