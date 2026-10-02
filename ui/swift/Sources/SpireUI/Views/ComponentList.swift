import SwiftUI

/// The **Components** of an ESP-IDF application.
///
/// A library's component list is where its product is edited — a component is added, written, removed.
/// An application is the other way round: it is *made of* its components — an actor per job, a stage per
/// edge of the data path, the message types they share — so this list is a reading of the tree, not a
/// place the tree grows. It is deliberately read-only: each row says what the component calls itself and
/// what it says it is, and nothing on this pane writes to any of them yet.
struct ComponentList: View {
    @Environment(AppTheme.self) private var theme

    let project: ProjectInfo

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
                        ComponentRow(component: component) { EmptyView() }
                    }
                }
            }
        }
    }
}

/// One component, as a line: its name, and what it says it is.
///
/// A component reads the same wherever it is listed — a library's product, an application's composition
/// — so the row is written once here and the two sections differ only in what hangs off its trailing
/// edge. A library offers Write and Remove (`ComponentSection`); an application offers nothing yet. The
/// framework badge is the same in both: a shipped component is not the caller's to change.
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
