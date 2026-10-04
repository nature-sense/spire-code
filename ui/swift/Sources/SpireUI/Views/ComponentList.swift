import SwiftUI

/// The **Components** of an ESP-IDF application.
///
/// A library's component list is where its product is edited — a component is added, written, removed.
/// An application is the other way round: it is *made of* its components — an actor per job, a stage per
/// edge of the data path, the message types they share — so the tree does not grow here. What a **unit**
/// of it (an actor or a stage) does carry is a **Write**: its code is written from the reviewed design
/// the application states in `SPIRE.application.json`, together with the framework's own rules. That is
/// the one thing on this pane that changes anything; a component that is not a unit — the shared types
/// in `components/messages/` — is a reading of the tree, and a framework component is the library's.
struct ComponentList: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    let project: ProjectInfo

    @State private var editing: SubprojectInfo?
    @State private var historyFor: SubprojectInfo?
    @State private var failure: String?
    @State private var outcome: String?

    /// A component is a subproject the analyzer labelled as one — not a guess from the path.
    private var components: [SubprojectInfo] {
        project.subprojects.filter { $0.kind == .component }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Components").font(.headline)

            if components.isEmpty {
                Text("None yet. An application is what its components make it — an actor per job, a "
                     + "stage per edge of the data path, and the message types they share.")
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
        .sheet(item: $editing) { unit in
            UnitEditSheet(unit: unit, root: project.root) { finish($0) }
        }
        .sheet(item: $historyFor) { unit in
            ComponentEditHistorySheet(root: project.root, component: unit.name)
        }
    }

    // MARK: - A component

    private func row(_ component: SubprojectInfo) -> some View {
        ComponentRow(component: component) {
            // Only a **unit** of the composition is written here: an actor or a stage, whose code comes
            // from the reviewed design the application carries. Everything else on this pane is read.
            if isUnit(component) {
                // The recorded edits for this unit: what was asked and what the gate did, readable
                // before or after a Write rather than only in its result.
                Button {
                    failure = nil
                    outcome = nil
                    historyFor = component
                } label: {
                    // Named, not a bare clock: the recorded edits are the only place the words typed into
                    // a Write are kept, so the way in has to be readable without hovering for it.
                    Label("History", systemImage: "clock.arrow.circlepath")
                        .labelStyle(.titleAndIcon)
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .help("Recorded edits for this unit")
                Button("Write") {
                    failure = nil
                    outcome = nil
                    editing = component
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Write this unit's code from the design the application carries")
            } else if component.componentFramework != nil {
                Label("shipped", systemImage: "lock")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
                    .help("Part of the library's framework: it comes with every library, and its code "
                          + "is not yours to change here.")
            }
            // Otherwise nothing hangs off the row: `components/messages/` is the shared types the
            // composition declares — it states `library`, and is not a unit the design gives a message
            // and a job to, so there is nothing here to write it from.
        }
    }

    /// A **unit** of the composition is an actor or a stage — the kinds the application's own fill wrote,
    /// and the only ones this pane can write. A driver or a library belongs to the *library* the
    /// application is built against, and `components/messages/` states no kind at all.
    private func isUnit(_ component: SubprojectInfo) -> Bool {
        component.componentFramework == nil
            && (component.componentKind == "actor" || component.componentKind == "stage")
    }

    private func finish(_ result: UnitEditSheet.Result) {
        switch result {
        case .ok(let instruction, let summary):
            // The box is gone once the sheet closes, so the words that were typed into it are put back
            // here, above what the run did. A blank box asks for nothing and contributes no line.
            let asked = instruction.trimmingCharacters(in: .whitespacesAndNewlines)
            outcome = asked.isEmpty ? summary : "You asked:\n\(asked)\n\n\(summary)"
            Task { await bridge.fetchProjectAnalysis() }
        case .failed(let message):
            failure = message
        }
    }
}

/// One component, as a line: its name, and what it says it is.
///
/// A component reads the same wherever it is listed — a library's product, an application's composition
/// — so the row is written once here and the two sections differ only in what hangs off its trailing
/// edge. A library offers Write, history and Remove (`ComponentSection`); an application offers Write and
/// history for its **units** (an actor or a stage), and nothing for the rest. The framework badge is the
/// same in both: a shipped component is not the caller's to change.
struct ComponentRow<Trailing: View>: View {
    @Environment(AppTheme.self) private var theme

    let component: SubprojectInfo
    let trailing: () -> Trailing

    init(component: SubprojectInfo, @ViewBuilder trailing: @escaping () -> Trailing) {
        self.component = component
        self.trailing = trailing
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .font(.caption)
                .foregroundStyle(theme.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text(component.name).font(.callout.weight(.semibold))
                Text(subtitle)
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            }
            Spacer(minLength: 0)
            trailing()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 1))
    }

    /// A shipped framework component reads as one thing; everything else reads as its kind.
    private var icon: String {
        if component.componentFramework != nil { return "square.stack.3d.up" }
        switch component.componentKind {
        case "library": return "function"
        case "actor": return "person.2"
        case "stage": return "arrow.left.arrow.right"
        default: return "shippingbox"
        }
    }

    /// What the component says it is — and, for the framework, that it is not the library's own work.
    ///
    /// From the component's own `CMakeLists.txt`, so it is shown rather than guessed at — and for one
    /// that states nothing (written by hand) the row says that, which is a fact too. The kind is the
    /// analyzer's reading (`componentKind`), so `actor` and `stage` are the application's own units and
    /// `driver` and `library` are the kinds a library adds.
    private var subtitle: String {
        if let framework = component.componentFramework {
            return "\(component.path) · the \(framework) framework · shipped with the library"
        }
        let what: String
        switch component.componentKind {
        case "driver": what = "a driver: one device, one bus"
        case "library": what = "a library: pure code, no bus"
        case "actor": what = "an actor: a message, its state, and a task of its own"
        case "stage": what = "a stage: what it pulls, what it pushes, and the code between"
        default: what = "states no kind"
        }
        return "\(component.path) · \(what)"
    }
}
