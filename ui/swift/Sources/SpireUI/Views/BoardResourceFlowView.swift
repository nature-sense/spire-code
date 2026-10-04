import SwiftUI

/// The experimental **board-and-resources** flow — v1 of the new UI, reachable only behind the
/// `experimentalFlow` flag so the existing wizard is untouched.
///
/// Two steps, each the minimum that makes the next one honest:
///
/// 1. **Board** — the boards the core knows, filtered to what a firmware application is built *for*.
///    Choosing one reads its resolved profile (`capabilities/profile`), because a board's parts are
///    facts the graph already holds and Swift must not re-derive them.
/// 2. **Resources** — one table, one row per **source**: every part the chosen board declares, then
///    every category of the capability vocabulary (`capabilities/vocabulary`). A row's first column is
///    the source's name; its second is the resources that source offers, and it stays **empty** until
///    the row is chosen. Choosing a **part** row is a choice — the part *is* the resource it offers,
///    so the row's own click puts it in and shows the resource selected and read-only; choosing a
///    **category** row only opens it, revealing its paths to tick, because a category holds many and
///    the application says which. Both feed one `resources:` set. Continue is **stubbed** for v1: it
///    shows the draft a later step turns into a spec.
///
/// What comes out is a **draft**, not a spec: a `library`/`published` resource still needs the
/// driver unit that only the design step knows, so nothing here claims to validate.
struct BoardResourceFlowView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    @State private var state = BoardResourceFlowState()
    @State private var step: Step = .board
    @State private var isLoading = true
    @State private var isChoosing = false
    @State private var showDraft = false
    /// Which table row is open — a board part or a catalogue category. Ephemeral: it is where the eye
    /// is, not part of the draft, so it lives in the view. An open **part** row is one whose part is in
    /// the draft; a **category** row is open until it is closed.
    @State private var selectedSource: Source?

    private enum Step { case board, resources }

    /// A row of the table: a **part** the board declares, or a catalogue **category**. Choosing a part
    /// row *is* selecting it (one part, one resource); choosing a category row only opens it, because a
    /// category holds many resources and the application must say which.
    private enum Source: Hashable {
        case part(String)
        case category(String)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header
            stepContent
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            footer
        }
        .padding(24)
        .frame(width: 700, height: 600, alignment: .topLeading)
        .background(theme.background)
        .task { await load() }
    }

    /// The current step, handed the space between the fixed header and footer. That **bounded** space is
    /// what lets the step's `ScrollView` actually scroll: a sheet that declares only a *minimum* height
    /// grows to fit its content instead, so the list overflows and its bottom is clipped with no way to
    /// reach it — which is why the catalogue's later categories were unreachable.
    @ViewBuilder
    private var stepContent: some View {
        switch step {
        case .board: boardStep
        case .resources: resourcesStep
        }
    }

    // MARK: - Load

    private func load() async {
        let platforms = await bridge.fetchPlatforms()
        state.boards = BoardResourceFlowState.boards(from: platforms)
        state.catalogue = await bridge.fetchVocabulary()
        isLoading = false
    }

    private func choose(_ board: Platform) {
        guard !isChoosing else { return }
        isChoosing = true
        Task {
            if let profile = await bridge.fetchBoardProfile(board: board.id) {
                state.choose(boardID: board.id, profile: profile)
                // Open with nothing chosen: every row's resources cell is empty, because a part only
                // goes into the draft when the application picks it.
                selectedSource = nil
                step = .resources
            }
            isChoosing = false
        }
    }

    /// Choose a row of the resources table.
    ///
    /// A **part** row is a choice: the part *is* the resource it offers, so clicking the row puts the
    /// part in (if it is not already) and opens it, showing its resource selected and read-only —
    /// there is nothing on that side to change. Clicking the *open* part row again takes the part back
    /// out, because the row is the only place the choice can be unmade.
    ///
    /// A **category** row only opens: a category holds many resources and the application says which, so
    /// clicking the row reveals its resources to tick and clicking it again closes it. What was ticked
    /// stays ticked — closing a row unchooses nothing.
    private func choose(_ source: Source) {
        switch source {
        case .part(let name):
            if selectedSource == .part(name) {
                state.toggleBoardPart(name: name)
                selectedSource = nil
            } else {
                if !state.isBoardPartSelected(name) {
                    state.toggleBoardPart(name: name)
                }
                selectedSource = .part(name)
            }
        case .category(let name):
            selectedSource = selectedSource == .category(name) ? nil : .category(name)
        }
    }

    // MARK: - Chrome

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Board & resources").font(.title3.weight(.semibold))
                Text(step == .board
                    ? "Choose the board this application is built for"
                    : "The devices this application composes against")
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
            }
            Spacer()
            Text(step == .board ? "1 of 2" : "2 of 2")
                .font(.caption)
                .foregroundStyle(theme.textTertiary)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if step == .resources {
                Button("Back") {
                    step = .board
                    showDraft = false
                }
                if !showDraft {
                    Text("\(state.resources.count) selected")
                        .font(.caption)
                        .foregroundStyle(theme.textTertiary)
                }
            }
            Spacer()
            Button("Close") { dismiss() }
            if step == .resources {
                Button("Continue") { showDraft = true }
                    .disabled(!state.canContinue)
            }
        }
    }

    // MARK: - Step 1: board

    private var boardStep: some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
            SectionHeading("Board")
            if isLoading {
                ProgressView().controlSize(.small)
            } else if state.boards.isEmpty {
                Text("No boards in the catalogue")
                    .font(.callout)
                    .foregroundStyle(theme.textTertiary)
            } else {
                ScrollView {
                    VStack(spacing: 6) {
                        ForEach(state.boards) { board in boardRow(board) }
                    }
                }
            }
        }
    }

    private func boardRow(_ board: Platform) -> some View {
        Button { choose(board) } label: {
            HStack(spacing: 10) {
                Image(systemName: "cpu").foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 2) {
                    Text(board.name).font(.callout.weight(.medium))
                    Text(board.id).font(.caption).foregroundStyle(theme.textSecondary)
                }
                Spacer(minLength: 8)
                if state.selectedBoardID == board.id {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(theme.accent)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isChoosing)
    }

    // MARK: - Step 2: resources

    private var resourcesStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            if let board = state.selectedBoard {
                Text(board.name).font(.title3.weight(.semibold))
            }

            if showDraft {
                draftStep
            } else {
                // One table, one row per source: a part the board carries, or a catalogue category.
                // The first column is the name; the second is that source's resources — and it
                // stays empty until the row is chosen, so the table is quiet with nothing selected. A
                // part row is a choice (one part is one resource, so choosing it puts the part in); a
                // category row only opens, because a category holds many paths and the application
                // says which.
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        sourceTable
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
    }

    /// The table: every part the board declares, then every category of the capability vocabulary,
    /// on one list. Each row is the same two columns — the source's name, then the resources that
    /// source offers — so a part and a category read the same way.
    private var sourceTable: some View {
        VStack(alignment: .leading, spacing: 0) {
            if selectedSource == nil {
                Text("Choose a board part or a category to see its resources.")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 6)
            }
            let parts = state.profile?.devices ?? []
            ForEach(parts) { device in
                sourceRow(.part(device.name), title: device.name, note: summary(for: device))
            }
            ForEach(state.catalogue) { category in
                sourceRow(
                    .category(category.category),
                    title: category.category,
                    note: "\(category.capabilities.count) capabilities"
                )
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// One table row. The first column is the **name** — and clicking it is the whole gesture: a
    /// **part** row is a choice, so clicking it opens the row and puts the part in (clicking the open
    /// row takes it out again); a **category** row only opens and closes. The second column is the
    /// **resources** of whatever is open, and it is empty for every row that is not.
    private func sourceRow(_ source: Source, title: String, note: String) -> some View {
        let open = selectedSource == source
        return HStack(alignment: .top, spacing: 0) {
            Button { choose(source) } label: {
                nameCell(source, title: title, note: note, open: open)
            }
            .buttonStyle(.plain)
            .frame(width: 196, alignment: .leading)

            Divider().overlay(theme.divider)

            Group {
                if open {
                    resourceCell(source)
                } else {
                    Color.clear.frame(height: 1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.leading, 12)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(open ? theme.accentBackground : Color.clear)
        .overlay(alignment: .bottom) { Divider().overlay(theme.divider) }
    }

    /// The first column of a row: what the source is called. A part carries a **check** here as well
    /// as in its resources cell — a part *is* the resource it offers, so the two agree; a category
    /// carries a chevron, because opening it is all it does.
    private func nameCell(_ source: Source, title: String, note: String, open: Bool) -> some View {
        HStack(spacing: 8) {
            sourceIcon(source, title: title, open: open)
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.callout.weight(.medium))
                if !note.isEmpty {
                    Text(note)
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
    }

    /// A row's leading mark: a check for a part (on when the part is in the draft), a chevron for a
    /// category (turned when the row is open).
    @ViewBuilder
    private func sourceIcon(_ source: Source, title: String, open: Bool) -> some View {
        switch source {
        case .part:
            Image(systemName: state.isBoardPartSelected(title) ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 13))
                .foregroundStyle(state.isBoardPartSelected(title) ? theme.accent : theme.textTertiary)
        case .category:
            Image(systemName: open ? "chevron.down" : "chevron.right")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(theme.textTertiary)
                .frame(width: 13)
        }
    }

    /// The second column of a row: the resources the source offers, and **nothing at all** until the
    /// row is chosen — the cell is empty, which is what keeps the table quiet.
    @ViewBuilder
    private func resourceCell(_ source: Source) -> some View {
        switch source {
        case .part(let name):
            if let device = state.profile?.devices.first(where: { $0.name == name }) {
                partResource(device)
            }
        case .category(let name):
            if let category = state.catalogue.first(where: { $0.category == name }) {
                categoryResources(category)
            }
        }
    }

    /// A part's resource — the one the board reaches for it — shown **selected and read-only**:
    /// the part *is* the resource, so there is nothing on this side to change. The row's own click is
    /// what put it in and what takes it out.
    private func partResource(_ device: BoardDevice) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 12))
                .foregroundStyle(theme.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text(device.name).font(.callout)
                Text(detail(for: device))
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            Text(ResourceVia.board.label)
                .font(.caption2)
                .foregroundStyle(theme.textTertiary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    /// A category's resources: every path it holds, **open and tickable** — the application's to
    /// pick. The catalogue is the *whole* vocabulary, so a path the board carries as a part still
    /// appears — read-only, with a `Board` badge — because the board's part row is its only control.
    /// That is what keeps the two sections in step: one capability is never a board part *and* a
    /// tickable addition.
    private func categoryResources(_ category: VocabularyCategory) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(category.capabilities, id: \.self) { path in
                catalogueRow(path)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The one-line summary a part carries in the first column: its part number when the board named
    /// one, else its role — short, because the row also shows the name.
    private func summary(for device: BoardDevice) -> String {
        if !device.part.isEmpty { return device.part }
        return device.role ?? ""
    }

    /// The same line for a part the board declares — its role, the part number it carries, and what
    /// it is reached on, read from the board's own `devices:` block.
    private func detail(for device: BoardDevice) -> String {
        var parts = [device.role ?? ""]
        if !device.part.isEmpty { parts.append(device.part) }
        if !device.bus.isEmpty { parts.append("bus: \(device.bus)") }
        if !device.address.isEmpty { parts.append("addr: \(device.address)") }
        return parts.filter { !$0.isEmpty }.joined(separator: " · ")
    }


    /// One catalogue path, as a checkbox in a category row's resources cell.
    ///
    /// Three states:
    /// * **board-owned** — the board declares a part for this role (`power.pmu` is the CoreS3's `pmu`),
    ///   so the path is **read-only**: a `Board` badge, an empty check until the part is selected, and
    ///   no click. The board's part row is its only control, which keeps this section in step with it —
    ///   one capability is never both a board part and a library addition.
    /// * **added** (`library`) — a check-mark; choosing it takes the addition back out.
    /// * **open** — an open checkbox; choosing it adds a `library` resource, named by the role's last
    ///   segment (`sensing.particulate` → `particulate`).
    ///
    /// Inlined rather than a `Menu`/`popover` for the same reason the whole catalogue is: those are
    /// transient macOS panels that do not reliably scroll their hosted content.
    private func catalogueRow(_ path: String) -> some View {
        let boardOwned = state.boardCarriesRole(path)
        let declared = state.declaredVia(role: path) != nil
        return Button {
            state.toggleCatalogueResource(role: path)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: declared ? "checkmark.circle.fill" : (boardOwned ? "circle" : "plus.circle"))
                    .font(.system(size: 12))
                    .foregroundStyle(declared ? theme.accent : theme.textTertiary)
                Text(path)
                    .font(.callout)
                    .foregroundStyle(declared ? theme.textTertiary : theme.textPrimary)
                Spacer(minLength: 8)
                if boardOwned {
                    Text(ResourceVia.board.label)
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                } else if declared {
                    Text(ResourceVia.library.label)
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(boardOwned)
    }

    // MARK: - The stubbed Continue

    private var draftStep: some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
            SectionHeading("Draft resources")
            Text("The spec step is not built yet; this is what the selection produces.")
                .font(.caption)
                .foregroundStyle(theme.textSecondary)
            ScrollView {
                Text(draft)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
                    .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            }
        }
    }

    private var draft: String {
        let entries = state.resources.map(\.json)
        let object: [String: Any] = ["resources": entries]
        guard
            let data = try? JSONSerialization.data(
                withJSONObject: object, options: [.prettyPrinted, .sortedKeys]),
            let text = String(data: data, encoding: .utf8)
        else {
            return ""
        }
        return text
    }
}

#Preview {
    BoardResourceFlowView()
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .frame(width: 720, height: 600)
}

