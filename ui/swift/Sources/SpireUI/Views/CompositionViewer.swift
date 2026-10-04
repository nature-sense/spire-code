import SwiftUI
import AppKit

/// Opens the composition viewer as its own floating dialog, the way the HAL docs, HAL verification and
/// fix panels are opened.
///
/// It is **not** a `.sheet`: this pane already carries a sheet of its own, and SwiftUI honours only one
/// sheet per view — a second one attached to the same view is silently dropped, which is why an opening
/// that presented it as a sheet did nothing. A viewer of the whole design is a read-only document beside
/// the work (like the HAL viewers), so a floating window is the honest shape for it anyway.
enum CompositionViewerPortal {
    private static var windows: [NSWindow] = []

    @MainActor static func open(bridge: SpireBridge, theme: AppTheme, root: String) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 640, height: 720),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Application design"; w.isReleasedWhenClosed = false
        let view = CompositionViewer(root: root).environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(NSSize(width: 640, height: 720))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 320, y: m.midY - 360)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// **What the application is made of** — the reviewed design an ESP-IDF application states, read back
/// from the file it carries, for reading only.
///
/// An application's units are *written from* the design the tree states (`ComponentList`'s Write), so the
/// architecture the code comes from is worth seeing beside the components that make it up, rather than
/// only through a file a person has to know to open. The reading is on top — the same `DesignReviewView`
/// the wizard approves a design with, because it is the same design — and beneath it the composition **as
/// the file holds it**: `composition.spire`, or the `SPIRE.application.json` an application scaffolded
/// before the composition existed carries. That is the text a Write is written from and the file a person
/// edits to change the design, so the panel names it.
///
/// It reads (`project/applicationDesign`) and writes nothing, so opening it is never a change.
struct CompositionViewer: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    let root: String

    @State private var design: ApplicationDesign?
    @State private var composition: String?
    @State private var file: String?
    @State private var loading = true
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Application design").font(.title3.weight(.semibold))
                Text("The composition this application is written from — read-only.")
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
            }

            if loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Reading the design…").font(.caption).foregroundStyle(theme.textSecondary)
                }
            } else if let failure {
                Text(failure).font(.caption).foregroundStyle(.red).textSelection(.enabled)
            } else if let design {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        DesignReviewView(design: design)
                        if let composition { compositionSection(composition) }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                Text("No design yet. Nothing in this tree states a composition, so there is no design the "
                     + "units are written from — the application has not been designed.")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(20)
        .frame(minWidth: 520, minHeight: 460, alignment: .topLeading)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(theme.background)
        .task { await load() }
    }

    /// The composition as the file holds it, named so a person knows which file to open to change it.
    private func compositionSection(_ text: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionHeading("Composition (\(file ?? "the file"))")
            Text(text)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(theme.textSecondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 1))
        }
    }

    private func load() async {
        let found = await bridge.fetchApplicationDesign(root: root)
        await MainActor.run {
            design = found.design
            composition = found.yaml
            file = found.file
            failure = found.error
            loading = false
        }
    }
}
