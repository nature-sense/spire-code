import SwiftUI

/// The **Components** section of an ESP-IDF component library.
///
/// A library's product *is* `components/*`, so this is where it grows: add a component, write its code,
/// remove it. The code is **generated** — the skeleton is a stub and the model fills in the part that
/// comes off a datasheet, or the algorithm that comes out of the user's head — so a component's Write
/// is a description box, not a code editor, and the honest thing to show about a run is *what was
/// checked*.
///
/// The three **framework** components are the exception to all of it: they come with the library, they
/// are complete, and their row has no Write and no Remove — see `SubprojectInfo.componentFramework`.
struct ComponentSection: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    let project: ProjectInfo

    @State private var adding = false
    @State private var editing: SubprojectInfo?
    @State private var removing: SubprojectInfo?
    @State private var busy: String?
    @State private var failure: String?
    @State private var outcome: String?

    /// A component is a subproject the analyzer labelled as one — not a guess from the path.
    private var components: [SubprojectInfo] {
        project.subprojects.filter { $0.kind == .component }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("Components").font(.headline)
                Spacer(minLength: 0)
                if busy != nil { ProgressView().controlSize(.small) }
                Button {
                    failure = nil
                    outcome = nil
                    adding = true
                } label: {
                    Label("Add", systemImage: "plus").font(.caption)
                }
                .buttonStyle(.bordered)
                .disabled(busy != nil)
            }

            if components.isEmpty {
                Text("None yet. The framework is here; what this library adds is on top of it — a "
                     + "driver, whose protocol is written from what you know about the device, or a "
                     + "library component, whose code is written from what you say it should do.")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
            } else {
                VStack(spacing: 6) {
                    ForEach(components) { component in
                        row(component)
                    }
                }
            }

            if let failure {
                Text(failure).font(.caption).foregroundStyle(.red).textSelection(.enabled)
            }
            if let outcome {
                Text(outcome)
                    .font(.caption2)
                    .foregroundStyle(theme.textSecondary)
                    .textSelection(.enabled)
            }
        }
        .sheet(isPresented: $adding) {
            ComponentSheet(mode: .add, root: project.root) { finish($0) }
        }
        .sheet(item: $editing) { component in
            ComponentSheet(mode: .edit(component), root: project.root) { finish($0) }
        }
        .alert(
            "Remove \(removing?.name ?? "")?",
            isPresented: Binding(
                get: { removing != nil },
                set: { if !$0 { removing = nil } }
            )
        ) {
            Button("Remove", role: .destructive) { remove() }
            Button("Cancel", role: .cancel) { removing = nil }
        } message: {
            Text("Its directory goes. If another component is built on it, the removal is refused "
                 + "rather than repairing the other component.")
        }
    }

    // MARK: - A component

    private func row(_ component: SubprojectInfo) -> some View {
        ComponentRow(component: component) {
            // A **framework** component has nothing to write and nothing to remove: it arrives
            // complete, it is what the library *is*, and both operations are refused if asked for
            // anyway. Offering the buttons would be inviting a refusal.
            if component.componentFramework == nil {
                Button("Write") {
                    failure = nil
                    outcome = nil
                    editing = component
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(busy != nil)
                Button {
                    failure = nil
                    outcome = nil
                    removing = component
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .disabled(busy != nil)
                .help("Remove this component")
            } else {
                Label("shipped", systemImage: "lock")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
                    .help("Part of the library's framework: it comes with every library, and neither "
                          + "its code nor its place is yours to change here.")
            }
        }
    }


    // MARK: - Acting

    private func remove() {
        guard let component = removing else { return }
        removing = nil
        busy = component.name
        Task {
            let (_, error) = await bridge.idfRemoveComponent(root: project.root, name: component.name)
            await MainActor.run {
                busy = nil
                if let error {
                    failure = error
                } else {
                    outcome = "Removed components/\(component.name)."
                    Task { await bridge.fetchProjectAnalysis() }
                }
            }
        }
    }

    private func finish(_ result: ComponentSheet.Result) {
        busy = nil
        switch result {
        case .ok(let summary):
            outcome = summary
            Task { await bridge.fetchProjectAnalysis() }
        case .failed(let message):
            failure = message
        }
    }
}
