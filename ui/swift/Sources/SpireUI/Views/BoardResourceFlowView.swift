import SwiftUI

/// The experimental **board-and-resources** flow — v1 of the new UI, reachable only behind the
/// `experimentalFlow` flag so the existing wizard is untouched.
///
/// Two steps, each the minimum that makes the next one honest:
///
/// 1. **Board** — the boards the core knows, filtered to what a firmware application is built *for*.
///    Choosing one reads its resolved profile (`capabilities/profile`), because a board's parts are
///    facts the graph already holds and Swift must not re-derive them.
/// 2. **Resources** — a two-column master–detail. The left column lists the **sources**: every part
///    the chosen board declares, then every category of the capability vocabulary
///    (`capabilities/vocabulary`). The right pane shows the selected source's resources as
///    checkboxes. Choosing a board part *is* selecting its one resource (the part is what the BSP
///    reaches, so the list row is its own checkbox); choosing a category only opens it, because a
///    category holds many paths and the application says which. Both feed one `resources:` set.
///    Continue is **stubbed** for v1: it shows the draft a later step turns into a spec.
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
    /// Which left-column row the right pane is showing — a board part or a catalogue category.
    /// Ephemeral: it is where the eye is, not part of the draft, so it lives in the view.
    @State private var selectedSource: Source?

    private enum Step { case board, resources }

    /// A row of the left column: a **part** the board declares, or a catalogue **category**. Choosing
    /// a part *is* selecting it (one part, one resource); choosing a category only opens it, because a
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
                // Open on the board's first part, so the right pane is not empty — showing it, not
                // selecting it: the draft still starts empty.
                selectedSource = profile.devices.first.map { .part($0.name) }
                step = .resources
            }
            isChoosing = false
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
                // Master–detail: the left column is the *source* — a part the board carries, or a
                // catalogue category — and the right pane is the resources that source offers. The
                // board/non-board distinction lives in the left column, so it is never inferred from
                // a check-mark.
                HStack(alignment: .top, spacing: 0) {
                    ScrollView { sourceList }
                        .frame(width: 236)
                    Divider()
                    ScrollView { resourcePane }
                        .frame(maxWidth: .infinity)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
    }

    /// The left column: every part the board declares, then every category of the capability
    /// vocabulary. A part is a **toggle** — one part is one resource, so choosing it *is* selecting
    /// it. A category only **opens** — it holds many resources, and the application says which.
    private var sourceList: some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
            SectionHeading("Board parts")
            let parts = state.profile?.devices ?? []
            if parts.isEmpty {
                Text("This board declares no parts")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
            } else {
                ForEach(parts) { device in
                    sourceRow(
                        .part(device.name),
                        title: device.name,
                        detail: summary(for: device),
                        check: state.isBoardPartSelected(device.name)
                    ) {
                        state.toggleBoardPart(name: device.name)
                        selectedSource = .part(device.name)
                    }
                }
            }

            SectionHeading("Categories").padding(.top, 10)
            ForEach(state.catalogue) { category in
                sourceRow(
                    .category(category.category),
                    title: category.category,
                    detail: "\(category.capabilities.count) capabilities",
                    check: nil
                ) {
                    selectedSource = .category(category.category)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 12)
    }

    /// One left-column row. A `check` makes it a **selectable part** (its state shown); no `check`
    /// makes it a plain **navigation** row (a category). The current source is tinted, so the right
    /// pane always has a visible origin.
    private func sourceRow(
        _ source: Source,
        title: String,
        detail: String,
        check: Bool?,
        action: @escaping () -> Void
    ) -> some View {
        let current = selectedSource == source
        return Button(action: action) {
            HStack(spacing: 8) {
                if let check {
                    Image(systemName: check ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 13))
                        .foregroundStyle(check ? theme.accent : theme.textTertiary)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(.callout.weight(.medium))
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 7)
                    .fill(current ? theme.accentBackground : theme.surface))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    /// The right pane: the resources of whatever the left column has open. A board part shows its one
    /// resource — shown selected, because choosing the part selected it. A category shows every path
    /// it holds, unselected, so the application picks.
    @ViewBuilder
    private var resourcePane: some View {
        switch selectedSource {
        case .part(let name):
            if let device = state.profile?.devices.first(where: { $0.name == name }) {
                partDetail(device)
            } else {
                pickHint
            }
        case .category(let name):
            if let category = state.catalogue.first(where: { $0.category == name }) {
                categoryDetail(category)
            } else {
                pickHint
            }
        case nil:
            pickHint
        }
    }

    private var pickHint: some View {
        Text("Choose a part or category to see its resources.")
            .font(.callout)
            .foregroundStyle(theme.textTertiary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 8)
    }

    /// A board part's one resource, shown **checked** because choosing the part chose it. Clicking the
    /// card takes the part back out — the same toggle the left row drives.
    private func partDetail(_ device: BoardDevice) -> some View {
        let selected = state.isBoardPartSelected(device.name)
        return VStack(alignment: .leading, spacing: 10) {
            SectionHeading("Resource")
            Button { state.toggleBoardPart(name: device.name) } label: {
                HStack(spacing: 10) {
                    Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 15))
                        .foregroundStyle(selected ? theme.accent : theme.textTertiary)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(device.name).font(.callout.weight(.semibold))
                        Text(detail(for: device))
                            .font(.caption)
                            .foregroundStyle(theme.textSecondary)
                    }
                    Spacer(minLength: 8)
                    Text(ResourceVia.board.label)
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 1))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            Text("A part of the board: the BSP reaches it, so no component of this project drives it.")
                .font(.caption)
                .foregroundStyle(theme.textTertiary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 12)
    }

    /// A category's resources: every path it holds, unselected unless the application chose it. The
    /// catalogue is the *whole* vocabulary, so a path the board already carries still appears — shown
    /// "on board" and disabled, so the same capability can never be declared twice.
    private func categoryDetail(_ category: VocabularyCategory) -> some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
            SectionHeading(category.category)
            VStack(spacing: 6) {
                ForEach(category.capabilities, id: \.self) { path in
                    catalogueRow(path)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 12)
    }

    /// The one-line summary a part carries in the left column: its part number when the board named
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


    /// One catalogue path, as a checkbox in the right pane.
    ///
    /// Three states, told by `declaredVia`:
    /// * **not declared** — an open checkbox; choosing it adds a `library` resource, named by the
    ///   role's last segment (`sensing.particulate` → `particulate`).
    /// * **added** (`library`) — a check-mark; choosing it takes the addition back out.
    /// * **on board** (`board`) — a check-mark, **disabled**: the board's own part already carries this
    ///   capability, so the catalogue cannot declare it again. `timekeeping.rtc` and `power.pmu` are on
    ///   the CoreS3, so they show that way. Inlined rather than a `Menu`/`popover` for the same reason
    ///   the whole catalogue is: those are transient macOS panels that do not reliably scroll their
    ///   hosted content.
    private func catalogueRow(_ path: String) -> some View {
        let via = state.declaredVia(role: path)
        let declared = via != nil
        let onBoard = via == .board
        return Button {
            state.toggleCatalogueResource(role: path)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: declared ? "checkmark.circle.fill" : "plus.circle")
                    .font(.system(size: 12))
                    .foregroundStyle(declared ? theme.accent : theme.textTertiary)
                Text(path)
                    .font(.callout)
                    .foregroundStyle(declared ? theme.textTertiary : theme.textPrimary)
                Spacer(minLength: 8)
                if declared {
                    Text(onBoard ? "on board" : ResourceVia.library.label)
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(onBoard)
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

