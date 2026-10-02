import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// The one thing a project type needs before it exists: **a name, and somewhere to put it**.
///
/// Not a wizard. The type was decided by the row that opened this, and asking again would be the
/// tree of questions the flat list replaced. What is left is a name and a location — and the location
/// is not even asked when the folder is already open, because an empty project being given a shape
/// knows where it lives. An ESP-IDF **application** gets one optional field more: the component
/// library it is built against. It is the one type whose product is a composition, and the one field
/// no other type has.
struct CreateProjectSheet: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    let type: ProjectType
    /// The `ProjectStructure` key the row carried, passed rather than read off `type` so this sheet
    /// cannot be opened for a row that has none.
    let structure: String
    /// The directory the project goes in. Folded into the root as `<location>/<name>`, except when
    /// it is the project itself — an open, empty folder being given a shape — and then it is the
    /// root unchanged and the sheet stops asking.
    let fixedLocation: String?

    @State private var name: String
    @State private var location: String?
    @State private var failure: String?
    /// **The core's own words, when the tree already had a design** — see
    /// [`ScaffoldSpec.designWarning`]. Not a failure: the tree keeps its design (that is the rule,
    /// and a design changes by editing `composition.spire`), so nothing is refused, undone or
    /// re-run. But the review step showed the *dropped* decomposition, and a person handed a project
    /// with no note has been told the wrong thing about their own project. Shown while the rest of
    /// the pipeline runs — a scaffold is instant and the fill and the build are not, so the notice
    /// sits on screen for the whole of it — and kept in [`creationLog`], which is the account of the
    /// run that outlives it.
    @State private var designWarning: String?
    @State private var isCreating = false
    @FocusState private var nameFocused: Bool

    /// The component library an ESP-IDF **application** is built against.
    ///
    /// Optional, and asked for here rather than assumed: an application that names no library still
    /// scaffolds — its `CMakeLists.txt` says "No library named" rather than reaching for a sibling
    /// that may not exist — but a library is the whole difference between an application that
    /// composes someone's components and one that has to grow its own.
    @State private var library: String?

    /// Where the wizard is. Every other type goes form → created; an ESP-IDF **application** goes
    /// form → designing → review → created, because its decomposition is decided *before* the tree it
    /// describes exists, and a person confirms it. That ordering is the whole point: a person says yes
    /// to a decomposition rather than to a pile of generated code.
    ///
    /// Not `private`: the primary button's rule is a pure function of the stage, and it is pinned by a
    /// test — see [`primaryEnabled`], which is where the rule that used to be missing lives.
    enum Stage { case form, designing, review, creating }

    @State private var stage: Stage = .form

    /// One line of [`creationLog`]: when it happened, what happened, and — on the line that closes a
    /// phase — how long that phase took.
    ///
    /// Kept as data rather than a pre-formatted string so the view can colour it and a test can pin
    /// the format without a view. The `kind` is what the line *is*, and it is also how it is drawn:
    /// a phase opening, a phase closing, an item that landed, or one that did not.
    struct CreationLogLine: Identifiable {
        enum Kind { case phase, done, ok, failed }

        let id = UUID()
        /// The wall-clock moment the line was written.
        let stamp: Date
        /// The line's text, without the stamp or the duration — those are the view's to add.
        let text: String
        /// The phase's own duration, on the line that closes it; `nil` on the line that opens one.
        let elapsed: TimeInterval?
        let kind: Kind

        /// `14:09:37  12s  Filled: 8 step(s) planned` — the one line the view draws.
        var rendered: String {
            var out = CreateProjectSheet.logStamp(stamp)
            if let elapsed { out += "  " + CreateProjectSheet.durationText(elapsed) }
            return out + "  " + text
        }
    }

    /// The in-view bound on [`creationLog`]. Long enough to hold a whole creation — a design, a fill,
    /// a scaffold and a first build's own lines — and short enough that a chatty stream cannot grow
    /// view state without bound. The file log is the unbounded one.
    static let logLimit = 400

    /// The design form: the board, and the six answers the decomposition is derived from.
    @State private var board = BoardChoice()
    @State private var designForm = ApplicationDesignForm()

    /// The board catalogue, as the board section presents it: the processors, and the boards each carries.
    ///
    /// Loaded once when the form appears (`platforms/list`, the same list the platforms screen reads).
    /// Empty means the fetch failed or the core is older than the `kind`/`chip` fields — and then the text
    /// fields are the only way in, because a picker with nothing in it is worse than a form.
    @State private var platforms: [Platform] = []
    /// The chosen board's catalogue id (`""` = none). Its BSP and its chip come from the row it names.
    @State private var selectedBoard = ""
    /// Whether the board is typed by hand rather than picked — for a board the catalogue does not have.
    @State private var boardIsCustom = false

    /// The design as it came back — for reading — and the spec as it must go back, **untouched**: what
    /// was reviewed is what gets scaffolded, with no round trip through the decoded type in between.
    @State private var design: ApplicationDesign?
    @State private var designSpec: [String: Any]?

    /// **The door the reviewed design came through**, named by the core and carried back with the spec
    /// it decided (`source`: `answers` when the form's answers pinned the framework, `model` when the
    /// model chose it, `composition_file` for a file a person wrote). The scaffold records it as the
    /// decision behind the composition it writes, and nothing in a composition says who chose it — so it
    /// travels beside the spec rather than being re-derived from it.
    @State private var designSource: String?

    /// Where a design came from, when it did not come from the six answers: the `composition.spire` a
    /// person opened. Shown **at review** — the step where the design is read, and so where it matters
    /// which file it came from — and cleared by `backToForm`, alongside the design itself.
    @State private var loadedComposition: String?

    /// The **goal** the fill is handed, when the design did not come from the form.
    ///
    /// The fill is told *why* this application exists beside the composition it is filling, and on the
    /// designed path that sentence is the form's own answers. A loaded file was never answered in this
    /// sheet, so it gives the file's own `justification` — which is exactly the sentence the composition
    /// states about itself — rather than the empty string a form nobody filled would.
    @State private var loadedGoal: String?

    /// What the creation pipeline is doing, as one word for the current phase. The phases are worth
    /// naming: a scaffold is instant and a fill is a model call, and a person watching deserves to
    /// know which one they are waiting on. The *history* of those phases — each with the time it
    /// took — is [`creationLog`], which is what turns a long wait into an account of where it went.
    @State private var progress: String?

    /// Every phase the pipeline has run, in order, as the sheet watched it.
    ///
    /// The single `progress` line answers "what is it doing *now*"; this answers the question a long
    /// wait actually raises — "what have the minutes gone on?". A design and a fill are model round
    /// trips and a first build is a compile, and none of the three says so on its own; a line per
    /// phase, with its stamp and its own duration, does. Bounded, because a first build streams a
    /// line at a time and view state is not a log file (the file is `spire-scaffold.log`).
    @State private var creationLog: [CreationLogLine] = []

    /// The last line the build streamed, so a first build that compiles the framework and whatever
    /// the composition pinned from source reads as forward motion rather than a still spinner. Fed
    /// by the same live stream the dashboard's Build panel consumes — see [`buildAndRepair`], which
    /// owns it for the length of the build and no longer.
    @State private var liveBuildLine: String?

    /// True only while the *first* build is running. It gates the note that explains why that build is
    /// slow, which is worth saying only for the build that pays for it — the rebuild after a repair
    /// reuses the work and finishes in seconds.
    @State private var firstBuild = false

    /// Why *this* first build is slow, said in terms of what the manifest actually pinned — written
    /// from the finalized dependency list, because before the manifest is finalized the answer is the
    /// scaffold's guess (the board's BSP unconditionally) rather than the composition's. `nil` until
    /// the dependencies are settled; the build only runs after they are.
    @State private var firstBuildNote: String?

    /// When a build line last reached [`creationLog`], which is what thins the build's stream to a
    /// readable cadence — see [`appendBuildLine`].
    @State private var lastBuildLogAt: Date?

    init(type: ProjectType, structure: String, fixedLocation: String? = nil) {
        self.type = type
        self.structure = structure
        self.fixedLocation = fixedLocation
        // An open folder already has a name — its own — so the field starts filled in rather than
        // empty. It is still editable: the directory and the project's name are different things.
        _name = State(initialValue: fixedLocation.map { ($0 as NSString).lastPathComponent } ?? "")
        _location = State(initialValue: fixedLocation)
        // An application starts with the shared library already chosen; every other type has no field
        // for one. Still editable — see `defaultLibrary`.
        _library = State(initialValue: Self.defaultLibrary(for: structure))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(type.title).font(.title3.weight(.semibold))
                Text(type.subtitle).font(.caption).foregroundStyle(theme.textSecondary)
            }

            switch stage {
            case .form:
                nameField
                if fixedLocation == nil { locationField }
                if isApplication { libraryField }
                if let root = resolvedRoot, nameIsValid {
                    Text("Creates \(root)")
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                if isApplication { designFormFields }
                // The design phase's other door, set under the six questions it is the alternative to:
                // a composition that already exists is a design already decided.
                if isApplication { loadCompositionField }
            case .designing:
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(progress ?? "Designing…")
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                }
                // The design is one model round trip, and it is the longest wait before anything is
                // written. The log is the only thing that says so while it runs.
                creationLogView
            case .review:
                // **Where the design came from**, when it was not designed here. The review is the same
                // review either way, and a person confirming a decomposition should be told which door
                // it arrived through — the file they opened is the thing to edit if they disagree with
                // any of it.
                if let loadedComposition {
                    Text("Opened from \(loadedComposition) — the design below is that file's.")
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
                if let design { DesignReviewView(design: design) }
                // A design with components to write and nowhere to write them. Not a refusal — the
                // person may have a library elsewhere — but the one thing about this design that a build
                // will complain about later, said now.
                if let design, library == nil, !design.componentsToWrite.isEmpty {
                    Text(
                        "No component library is chosen, and this design has to write "
                            + "\(design.componentsToWrite.count) component"
                            + (design.componentsToWrite.count == 1 ? "" : "s")
                            + ": \(design.componentsToWrite.map(\.id).joined(separator: ", ")). "
                            + "Send the design back to choose one, or add it before the build."
                    )
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                }
            case .creating:
                if let root = resolvedRoot, nameIsValid {
                    Text("Creates \(root)")
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
                HStack(spacing: 8) {
                    if isCreating { ProgressView().controlSize(.small) }
                    Text(progress ?? "Creating…")
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                }
                // **Why the wait is long, and what it is doing right now.** A first ESP-IDF build
                // compiles the framework — and, when the composition pinned one, a board's BSP and
                // its whole peripheral stack with it — from source, so it costs minutes where every
                // later build costs seconds. Said in the terms the *finalized* manifest settled,
                // because a sensor-only composition that pins nothing managed is the framework alone
                // and naming a BSP it never pulled is the kind of note a person stops believing.
                if firstBuild, let firstBuildNote {
                    Text(firstBuildNote)
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let liveBuildLine {
                    Text(liveBuildLine)
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(theme.textTertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
                // The account of the wait: every phase, its stamp, and how long it took. This is what
                // the sheet was missing — a still spinner for a model call and a still spinner for a
                // build look the same, and the log is the one thing that tells them apart.
                creationLogView
            }

            if let failure {
                Text(failure)
                    .font(.caption)
                    .foregroundStyle(.red)
                    // A build failure carries the toolchain's own text, and the note that names its fix
                    // — `make run-idf` — sits at the *end* of it, so the cap has to leave room for the
                    // whole diagnosis rather than cutting the one line worth acting on. The text is
                    // selectable and copies whole even where the sheet cannot draw all of it.
                    .lineLimit(24)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }

            // **The core says the tree already had a design.** Not a failure — the tree keeps its
            // design and the pipeline carries on — but the one thing about this run a person has to
            // know, so it is drawn in the same place as a failure and in the same voice: amber, and
            // selectable so it can be read and quoted rather than only glanced at.
            if let designWarning {
                Text(designWarning)
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }

            HStack(spacing: 8) {
                if isCreating {
                    ProgressView().controlSize(.small)
                }
                Spacer(minLength: 0)
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(isCreating)
                // A design is a proposal, not a commitment: sending it back is a first-class action
                // rather than a cancel-and-start-over, because the form's answers are still there.
                if stage == .review {
                    Button("Design again") { backToForm() }
                        .disabled(isCreating)
                }
                Button(primaryLabel) { primaryAction() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canProceed)
            }
        }
        .padding(20)
        .frame(width: isApplication ? 560 : 460)
        .background(theme.background)
        .onAppear { nameFocused = true }
    }

    // MARK: - The form

    private var nameField: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionHeading("Name")
            TextField("my-project", text: $name)
                .textFieldStyle(.roundedBorder)
                .focused($nameFocused)
        }
    }

    private var locationField: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionHeading("Location")
            HStack(spacing: 8) {
                Text(location ?? "Not chosen")
                    .font(.caption)
                    .foregroundStyle(location == nil ? theme.textTertiary : theme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button("Choose…") { chooseLocation() }
            }
            Text("Required: the project is written there, as <location>/<name>.")
                .font(.caption2)
                .foregroundStyle(theme.textTertiary)
        }
    }

    private var libraryField: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionHeading("Component library")
            HStack(spacing: 8) {
                Text(library ?? "None — this application grows its own components")
                    .font(.caption)
                    .foregroundStyle(library == nil ? theme.textTertiary : theme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button("Choose…") { chooseLibrary() }
            }
        }
    }

    // MARK: - The form

    /// The board: **chosen from the catalogue**, not typed.
    ///
    /// Three free-text fields with no validation is how three runs in a row went wrong — a BSP the model
    /// rewrote, a wrong chip, and a stray comma that became a dependency key the component manager could
    /// not resolve. Everything those fields asked for is already known: `chips/` and `boards/` hold the
    /// processors and the boards that carry them (each board declares its `chip:`), and the app already
    /// reads them as `Platform`s — `platforms/list`, the same list the platforms screen shows.
    ///
    /// So it is a selection — **processor → board** — and the board brings its chip and its BSP with it.
    /// A board with no published BSP is an ordinary answer: the row says so, and the core generates its
    /// own backend for it. There is deliberately **no HAL level**: the HAL was chip-scoped and is retired,
    /// and under ESP-IDF the HAL *is* IDF, so there is nothing to enumerate.
    ///
    /// "Custom board…" keeps the three fields for a board the catalogue does not have — and every one of
    /// them is still checked where it is used, by `board_from_json`, which refuses what cannot be a name.
    private var boardFields: some View {
        VStack(alignment: .leading, spacing: 8) {
            if catalogueAvailable && !boardIsCustom {
                labelledPicker("Processor", selection: $board.chip) {
                    Text("Choose…").tag("")
                    ForEach(chips) { chip in Text(chip.name).tag(chip.id) }
                }
                labelledPicker("Board", selection: $selectedBoard) {
                    Text("Choose…").tag("")
                    ForEach(boardsForChip) { candidate in Text(candidate.name).tag(candidate.id) }
                }
                .disabled(board.chip.isEmpty)
                Text(boardSummary)
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
                    .textSelection(.enabled)
            } else {
                HStack(spacing: 8) {
                    TextField("esp32s3", text: $board.chip)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 110)
                    TextField("espressif/m5stack_core_s3", text: $board.bsp)
                        .textFieldStyle(.roundedBorder)
                    TextField("hal (optional)", text: $board.hal)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 130)
                }
            }
            Toggle("Custom board…", isOn: $boardIsCustom)
                .toggleStyle(.checkbox)
                .font(.caption)
                .disabled(!catalogueAvailable)
            Text(boardHint)
                .font(.caption2)
                .foregroundStyle(theme.textTertiary)
        }
        .task {
            if platforms.isEmpty { platforms = await bridge.fetchPlatforms() }
        }
        .onChange(of: board.chip) { _, _ in
            // A different processor is a different set of boards, so the board selection cannot survive it.
            guard !boardIsCustom else { return }
            selectedBoard = ""
            board.bsp = ""
            board.hal = ""
        }
        .onChange(of: selectedBoard) { _, _ in
            guard !boardIsCustom, let picked = boardsForChip.first(where: { $0.id == selectedBoard })
            else { return }
            // The board brings its own facts: the chip it carries, and its BSP when it has one.
            board.chip = picked.chip ?? board.chip
            board.bsp = picked.bsp ?? ""
            board.hal = ""
        }
    }

    /// One row of the cascade: a label, then the picker.
    private func labelledPicker<Content: View>(
        _ label: String,
        selection: Binding<String>,
        @ViewBuilder content: () -> Content
    ) -> some View {
        HStack(spacing: 8) {
            Text(label)
                .font(.caption)
                .frame(width: 68, alignment: .leading)
            Picker("", selection: selection, content: content)
                .labelsHidden()
                .frame(maxWidth: 260, alignment: .leading)
        }
    }

    /// The **processors** an application can be designed for: the catalogue's chips, filtered to the SDK
    /// this wizard's applications are in — a Linux SBC's processor is not one an ESP-IDF application runs
    /// on.
    private var chips: [Platform] {
        platforms
            .filter { $0.kind == "chip" && $0.os == "esp-idf" }
            .sorted { $0.name < $1.name }
    }

    /// The boards carrying the selected processor. A board's own `chip:` is the link, and it is the same
    /// spelling as the chip's `id`, so the filter is the join.
    private var boardsForChip: [Platform] {
        platforms
            .filter { $0.kind == "board" && ($0.chip ?? "") == board.chip }
            .sorted { $0.name < $1.name }
    }

    /// Whether the catalogue arrived. If it did not, the text fields are the only way in — a picker with
    /// nothing in it is worse than a form.
    private var catalogueAvailable: Bool { !chips.isEmpty }

    /// What the chosen board carries — its BSP, or the fact that it has none, which is a real answer.
    private var boardSummary: String {
        guard let picked = boardsForChip.first(where: { $0.id == selectedBoard }) else {
            return board.chip.isEmpty
                ? "Pick a processor, then the board it carries."
                : "\(boardsForChip.count) board(s) for this processor."
        }
        let carries: String
        if let bsp = picked.bsp, !bsp.isEmpty {
            carries = "BSP `\(bsp)` — a managed dependency"
        } else {
            carries = "no published BSP — Spire generates its own backend"
        }
        return "\(picked.name): \(carries)"
    }

    private var boardHint: String {
        catalogueAvailable && !boardIsCustom
            ? "Chosen from the board catalogue. A board with no BSP is fine — Spire generates its own "
                + "backend for it."
            : "A chip is required; a BSP only when the board has one. Both are checked before the design "
                + "runs."
    }

    /// The board, and the six questions the decomposition is derived from.
    ///
    /// The questions are asked here rather than left to a free-text box because the design request puts
    /// them to the model by name: a model handed six labelled answers can *see* which one is missing and
    /// say so, instead of inventing a device, an address or a protocol to fill the gap. Empty answers are
    /// simply not sent.
    private var designFormFields: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHeading("Board")
            boardFields

            SectionHeading("What it does")
            TextEditor(text: $designForm.purpose)
                .font(.caption)
                .frame(height: 52)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(theme.textTertiary.opacity(0.3)))

            SectionHeading("What it senses")
            TextField("sensors, buttons, network, time — and roughly how fast", text: $designForm.senses)
                .textFieldStyle(.roundedBorder)

            SectionHeading("What it acts on")
            TextField("display, audio, motor, actuator, network", text: $designForm.actsOn)
                .textFieldStyle(.roundedBorder)

            SectionHeading("What it reacts to over time")
            TextField("periodic? on-command? on-threshold? on-event?", text: $designForm.reactsTo)
                .textFieldStyle(.roundedBorder)

            SectionHeading("Timing")
            TextField("what is parallel, what is human-rate, what is machine-rate", text: $designForm.timing)
                .textFieldStyle(.roundedBorder)

            SectionHeading("When something is missing")
            TextField("a sensor absent, WiFi down, battery low", text: $designForm.missing)
                .textFieldStyle(.roundedBorder)
        }
    }

    /// **The design phase's other door**: a composition a person already has.
    ///
    /// The six answers above are one way to arrive at a decomposition; a `composition.spire` is the
    /// other, and the better one when it already exists — it *is* the file the scaffold writes and a
    /// project is edited through, so opening it and reviewing it is the same review, by the same rules.
    /// It skips the model entirely: nothing is designed, because the design is already written down.
    ///
    /// The library field above still applies: a composition that writes components writes them into a
    /// library, and this is where it is named.
    private var loadCompositionField: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionHeading("Or load a composition")
            HStack(spacing: 8) {
                Text("None opened — answer the six questions, or open one you already have")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button("Open…") { loadComposition() }
                    // **The same rule as the primary button**, for the same reason: the review step has
                    // no location field, so a composition opened before the project has a name and a
                    // place would land on a Create button that can never enable.
                    .disabled(!canCreate)
            }
            Text(
                canCreate
                    ? "A `composition.spire` — the file the scaffold writes and a person edits. "
                        + "Opening one skips the design call and goes straight to its review."
                    : "Name the project and choose where it goes first: the review step has no "
                        + "location field."
            )
            .font(.caption2)
            .foregroundStyle(canCreate ? theme.textTertiary : theme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - The creation log

    /// The log, as the sheet draws it: one monospaced line per phase, newest at the bottom.
    ///
    /// Auto-scrolled, because the interesting line is always the last one; selectable, because the
    /// reason a creation stopped is often a line a person wants to paste somewhere. Hidden while
    /// there is nothing to say, so the stages that never create do not grow a panel for no reason.
    @ViewBuilder
    private var creationLogView: some View {
        if !creationLog.isEmpty {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 1) {
                        ForEach(creationLog) { line in
                            Text(line.rendered)
                                .font(.system(.caption2, design: .monospaced))
                                .foregroundStyle(colour(for: line.kind))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .id(line.id)
                        }
                    }
                    .padding(8)
                }
                .frame(height: 180)
                .background(
                    RoundedRectangle(cornerRadius: 4)
                        .fill(theme.textTertiary.opacity(0.06))
                )
                .onChange(of: creationLog.last?.id) { _, _ in
                    // Keyed on the newest line's identity, not the count: once the log is at its cap
                    // the count stops changing and a count-keyed scroll would stop following it.
                    guard let last = creationLog.last else { return }
                    proxy.scrollTo(last.id, anchor: .bottom)
                }
            }
        }
    }

    /// How a log line is drawn: a failure is the one thing worth a colour, and a phase is told apart
    /// from its own result so the eye can find the boundaries.
    private func colour(for kind: CreationLogLine.Kind) -> Color {
        switch kind {
        case .phase: return theme.textSecondary
        case .done, .ok: return theme.textTertiary
        case .failed: return .red
        }
    }

    /// Open a phase: one line saying what is starting, and the clock it started on.
    ///
    /// Returns the start, which the caller hands back to [`finish`] so the phase's own duration lands
    /// on the line that closes it. Called on the main actor — every phase boundary already is.
    @discardableResult
    private func open(_ text: String) -> Date {
        appendLogLine(text, kind: .phase)
        progress = text
        return Date()
    }

    /// Close a phase: one line saying how it ended, and how long it took.
    private func finish(_ text: String, since start: Date, kind: CreationLogLine.Kind = .done) {
        appendLogLine(text, kind: kind, elapsed: Date().timeIntervalSince(start))
    }

    /// Append one line, bounded.
    ///
    /// A repeated line is dropped: the build's consumer is called on every poll and the line it
    /// carries rarely changes between two of them, so an un-deduped log would be mostly one line
    /// repeated. The oldest lines fall off the front past [`logLimit`], because the newest are the
    /// ones worth the room.
    private func appendLogLine(
        _ text: String,
        kind: CreationLogLine.Kind = .done,
        elapsed: TimeInterval? = nil
    ) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, creationLog.last?.text != trimmed else { return }
        creationLog.append(CreationLogLine(stamp: Date(), text: trimmed, elapsed: elapsed, kind: kind))
        if creationLog.count > Self.logLimit {
            creationLog.removeFirst(creationLog.count - Self.logLimit)
        }
    }

    /// A build line, thinned for the log: at most one every couple of seconds.
    ///
    /// The log is a phase story — design, fill, verify, build — and a compile has something to say
    /// every fraction of a second, so transcribing the stream would push the phases that took the
    /// *model calls* off the top within one build. The streamed lines reach the log as a sample; the
    /// pinned line above it is the unthinned "right now", and the file log is the full record.
    private func appendBuildLine(_ text: String) {
        let now = Date()
        if let last = lastBuildLogAt, now.timeIntervalSince(last) < 2 { return }
        lastBuildLogAt = now
        appendLogLine(text, kind: .ok)
    }

    // MARK: - The design phase

    /// Ask for a design, from the board and the six answers.
    ///
    /// Nothing is written: the design phase happens **before** the tree it describes, which is what
    /// makes it reviewable — a person says yes to a decomposition rather than to a pile of generated
    /// code. The call is one model round trip; the six rules and the repair turn are the core's.
    /// The framework is **not** pinned from the form, deliberately: the model chooses it after reading
    /// the answers and has to justify the choice, and the person confirms it at review. That is the
    /// derived-and-confirmed shape — chosen where it can be informed, overridable where it is read.
    private func askForDesign() {
        failure = nil
        designWarning = nil
        // A design asked for here **replaces** a composition that was opened: the six answers are the
        // design now, so the goal the fill is handed is theirs rather than a file's justification.
        loadedComposition = nil
        loadedGoal = nil
        stage = .designing
        // A design is a new run: the log starts here, so a person reads this attempt's phases rather
        // than a previous one's. It is the longest wait before anything is written, which is exactly
        // why it has to say what it is doing.
        creationLog = []
        let description = designForm.description
        let boardJSON = board.json
        let libraryRoot = library
        let started = open("Designing the composition…")
        Task {
            let (design, spec, source, error) = await bridge.designApplication(
                board: boardJSON,
                description: description,
                framework: nil,
                libraryRoot: libraryRoot
            )
            await MainActor.run {
                if let design, let spec {
                    self.design = design
                    self.designSpec = spec
                    // The door the core named for a design asked for here: the form pins no framework,
                    // so the model chose and justified one. Handed back to the scaffold, which records
                    // it as the decision behind the composition.
                    self.designSource = source
                    finish(
                        "Designed: \(design.framework), \(design.units.count) unit(s), "
                            + "\(design.componentsToWrite.count) to write",
                        since: started
                    )
                    stage = .review
                } else {
                    // The design phase leaves the wizard where it was: nothing was written, so nothing
                    // has to be undone, and the form's answers are still there to adjust.
                    let why = error ?? "the design came back empty"
                    finish("Design failed: \(why)", since: started, kind: .failed)
                    failure = why
                    stage = .form
                }
            }
        }
    }

    /// The one project type whose product is a *composition*: an ESP-IDF application is built
    /// against a component library, and naming it is what lets the library's `SPIRE.md` reach the
    /// model when the project is filled. Every other type is complete on its own.
    private static let applicationStructure = "idf_application"

    /// The library an ESP-IDF **application** is built against by default: the shared `spire-idf` under
    /// the user's home, where this machine's framework and own drivers live. `nil` for every other type
    /// — they have no library field at all.
    ///
    /// A default rather than a blank, because the ordinary application *does* build on that library: an
    /// application that names no library has to grow its own copy of the framework and the drivers, which
    /// is the outcome the field exists to avoid. It stays editable — a library elsewhere is one click
    /// away.
    static func defaultLibrary(for structure: String) -> String? {
        guard structure == applicationStructure else { return nil }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("naturesense/spire/spire-idf").path
    }

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// A name has to be a name. A slash would turn it into a path — `<location>/a/b` — and the
    /// project would not be where the sheet said it would be.
    private var nameIsValid: Bool {
        !trimmedName.isEmpty && !trimmedName.contains("/") && !trimmedName.hasPrefix(".")
    }

    /// The directory the scaffold is written into. The core creates it if it does not exist, so this
    /// is `<location>/<name>` even on the first run.
    private var resolvedRoot: String? {
        if let fixedLocation { return fixedLocation }
        guard let location, nameIsValid else { return nil }
        return (location as NSString).appendingPathComponent(trimmedName)
    }

    private var canCreate: Bool { resolvedRoot != nil && nameIsValid && !isCreating }

    /// Whether this sheet is creating a *composition* — the one type whose product is decided before
    /// its tree exists, and so the one type that gets a design phase.
    private var isApplication: Bool { structure == Self.applicationStructure }

    /// What the primary button says. An application is **designed** first; the design is what a person
    /// approves, and creating is what follows a yes.
    private var primaryLabel: String {
        if !isApplication { return "Create" }
        return stage == .review ? "Create" : "Design"
    }

    /// The primary button's rule, as a **pure function** — `false` disables it.
    ///
    /// The form needs **a place to put the project**, not only a board and the answers, and that is the
    /// half that was missing: a design ran happily with no location chosen, and then the *review*
    /// screen — which has no location field — offered a Create button that could never enable. Naming
    /// the rule here and requiring the root *before* designing makes the two screens agree, instead of
    /// leaving a person to discover the difference with a greyed-out button.
    static func primaryEnabled(
        stage: Stage,
        isApplication: Bool,
        isCreating: Bool,
        boardComplete: Bool,
        formUsable: Bool,
        canCreate: Bool
    ) -> Bool {
        if isCreating { return false }
        if !isApplication { return canCreate }
        switch stage {
        case .form:
            // A board, something to design from, **and somewhere to put the result**: the core refuses
            // a design without the first two, and the third is what the review screen would otherwise
            // silently require.
            return canCreate && boardComplete && formUsable
        case .review:
            return canCreate
        default:
            return false
        }
    }

    private var canProceed: Bool {
        Self.primaryEnabled(
            stage: stage,
            isApplication: isApplication,
            isCreating: isCreating,
            boardComplete: board.isComplete,
            formUsable: designForm.isUsable,
            canCreate: canCreate
        )
    }

    private func primaryAction() {
        guard isApplication else { return create() }
        if stage == .review { create() } else { askForDesign() }
    }

    private func backToForm() {
        stage = .form
        design = nil
        designSpec = nil
        designSource = nil
        failure = nil
        designWarning = nil
        // A design sent back is not the file that was opened: the form is where the next one comes
        // from, so what that file would have carried is cleared with it.
        loadedComposition = nil
        loadedGoal = nil
    }

    private func chooseLocation() {
        NSApp.activate(ignoringOtherApps: true)

        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        panel.message = "Choose the folder the project goes in"
        panel.directoryURL = location.map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser

        if panel.runModal() == .OK, let url = panel.url {
            location = url.path
        }
    }

    /// A library is a directory, like the location — its `SPIRE.md` is read from the root of it.
    private func chooseLibrary() {
        NSApp.activate(ignoringOtherApps: true)

        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        panel.prompt = "Choose"
        panel.message = "Choose the component library this application is built against"
        panel.directoryURL = location.map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser

        if panel.runModal() == .OK, let url = panel.url {
            library = url.path
        }
    }

    /// **Open a `composition.spire`** and take it to review — the design phase, without the design call.
    ///
    /// The parse and the six rules are the **core's** (`createProject/ParseComposition`), deliberately:
    /// a second YAML reader in Swift would be a second, drifting answer to what a composition is, and
    /// the rule that matters is that a file opened here is held to exactly what one on a project tree
    /// is. A file that does not parse, or does not hold together, is refused by name and the wizard
    /// stays on the form — nothing was written, and the file is the thing to fix.
    ///
    /// A successful open lands on the **review** step, which is the same review either door leads to:
    /// what a person approves is what gets scaffolded.
    private func loadComposition() {
        NSApp.activate(ignoringOtherApps: true)

        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Open"
        panel.message = "Choose the composition to review"
        panel.directoryURL = location.map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser
        // `.spire` is unregistered on a fresh machine, so a `composition.spire` is undifferentiated
        // data to the system — filtering to `.yaml`/`.text` would grey out the very file this is for.
        // So the filter is asked for only when the system knows the type, and otherwise the panel is
        // left open: the core refuses anything that is not a composition, by name, which is a better
        // answer than a file a person cannot select.
        if let spire = UTType(filenameExtension: "spire") {
            panel.allowedContentTypes = [spire, .yaml, .json, .plainText, .text]
        }

        guard panel.runModal() == .OK, let url = panel.url else { return }
        let name = url.lastPathComponent
        let text: String
        do {
            text = try String(contentsOf: url, encoding: .utf8)
        } catch {
            failure = "\(name) could not be read: \(error.localizedDescription)"
            return
        }

        // A load is a run of its own, and it opens with where the design came from: the phases that
        // follow are about a composition nobody designed in this sheet.
        failure = nil
        designWarning = nil
        creationLog = []
        let started = open("Opening \(name)…")
        Task {
            let (design, spec, source, error) = await bridge.parseComposition(text: text, name: name)
            await MainActor.run {
                guard let design, let spec else {
                    let why = error ?? "the composition came back empty"
                    finish("Opening failed: \(why)", since: started, kind: .failed)
                    failure = why
                    return
                }
                // The spec goes back to the core **untouched**: what is reviewed is what is scaffolded.
                self.design = design
                self.designSpec = spec
                // …and so does the door the core named for this one: a file a person wrote.
                self.designSource = source
                self.loadedComposition = name
                // The file's own justification is the goal the fill is handed: a composition states
                // what it is, and the form's answers — the usual goal — were never filled in.
                let justification = design.justification?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                self.loadedGoal = (justification?.isEmpty == false) ? justification : nil
                finish(
                    "Opened: \(design.framework), \(design.units.count) unit(s), "
                        + "\(design.componentsToWrite.count) to write",
                    since: started
                )
                stage = .review
            }
        }
    }

    /// Create it. For a native project that is the scaffold itself; for an **application** it is the
    /// pipeline the design phase earns: apply the design to the library, scaffold the application with
    /// the spec that was approved, fill it from that spec, and execute the fill.
    private func create() {
        guard let root = resolvedRoot else { return }
        failure = nil
        // A creation is its own run: any report from the previous one is about a tree that is not
        // this one. The next scaffold says what *it* found, and the notice cannot outlive the attempt.
        designWarning = nil
        isCreating = true
        stage = .creating
        // A creation that did not come through the design phase is its own run; one that did keeps
        // the log the design already started, so the whole story reads in one place.
        if !isApplication { creationLog = [] }
        progress = "Scaffolding…"
        let spec = designSpec
        // The door the reviewed design came through, carried back with the spec it decided: the scaffold
        // records it as the decision behind the composition this run writes. `nil` (a design the core did
        // not name a door for) sends nothing, and the core records no decision rather than a guessed one.
        let designSource = designSource
        let libraryRoot = library
        // The platform the scaffold and the build both run for: the **design's** chip, since a
        // composition opened from a file left the form's picker empty. One rule, so the scaffold's
        // recorded targets and the build's module cannot disagree.
        let chip = Self.buildPlatform(designChip: design?.board.chip, formChip: board.chip)
        let scaffoldPlatforms = chip.map { [$0] } ?? []
        // The decision, written down. Two binaries can disagree about this — a create run inside a
        // process launched before a fix was linked reads the old rule — and the only way to tell the
        // two apart from the logs is to state the inputs and what they resolved to. `nil` here with a
        // chip visible in the tree means the *build* is stale, not the rule.
        SpireBridge.logScaffold(
            Self.buildPlatformLogLine(designChip: design?.board.chip, formChip: board.chip)
        )
        // The goal the fill is handed: the form's answers on the designed path, and the composition's
        // own `justification` when the design was **opened** from a file — a form nobody filled in has
        // nothing to say about an application that is already written down.
        let goal = loadedGoal ?? designForm.description
        Task {
            // **The design reaches the library first.** A stub the design asked for has to exist — with
            // its manifest, so the application's `REQUIRES` resolves — before the application's tree is
            // written. A library that could not take a component is reported, not swallowed: the
            // application would otherwise be scaffolded against a component nobody has.
            if let spec, let libraryRoot, !libraryRoot.isEmpty {
                let started = await MainActor.run { open("Applying the design to the library…") }
                let (result, error) = await bridge.applyDesignedComponents(
                    libraryRoot: libraryRoot,
                    application: spec
                )
                if let error {
                    await MainActor.run {
                        finish("Library refused the design: \(error)", since: started, kind: .failed)
                        failure = error
                        isCreating = false
                        stage = .review
                    }
                    return
                }
                // **The design and the library disagree.** A component the design marks `existing` that
                // the library does not have comes back in `problems`, not `error`: the library
                // directory is written either way, and the scaffold that follows names the missing
                // component in the application's `REQUIRES` — so what *would* fail is the build,
                // minutes later, on a raw CMake line that never mentions the design. The answer was in
                // hand before a file was written, so the sheet stops on it here instead.
                let problems = (result?["problems"] as? [String]) ?? []
                if let disagreement = Self.designLibraryDisagreement(problems) {
                    await MainActor.run {
                        progress = nil
                        finish(
                            "The library and the design disagree: \(problems.count) problem(s)",
                            since: started,
                            kind: .failed
                        )
                        failure = disagreement
                        isCreating = false
                        stage = .review
                    }
                    return
                }
                await MainActor.run {
                    finish("Library: \(addedSummary(result))", since: started)
                    if let next = result?["next"] as? String { progress = next }
                }
            }

            // The structure routes everything: the config file, the build module and the build-system
            // label all follow from the key the row carried. The language never reaches an ESP-IDF
            // project — both types are "ESP-IDF", neither is "C++" — so "cpp" says what its files
            // are, not what decides anything.
            let scaffoldStarted = await MainActor.run { open("Scaffolding…") }
            let error = await bridge.scaffoldProject(
                buildSystem: "cpp",
                projectName: trimmedName,
                root: root,
                platforms: scaffoldPlatforms,
                structure: structure,
                embeddedRoot: libraryRoot,
                application: spec,
                designSource: designSource
            )
            if let error {
                await MainActor.run {
                    finish("Scaffold failed: \(error)", since: scaffoldStarted, kind: .failed)
                    failure = error
                    isCreating = false
                    stage = spec == nil ? .form : .review
                }
                return
            }
            await MainActor.run {
                finish("Scaffolded", since: scaffoldStarted)
                // **The core's own report, when the tree it wrote already stated a design.** Not a
                // failure: the tree's design is the one that decides — a design changes by editing
                // `composition.spire` — and every step below reads that same one, which is exactly
                // what keeps the tree's `REQUIRES`, its record and its composition from disagreeing.
                // So the run carries on, nothing is undone and nothing is re-run. What it is *not* is
                // silent: the review step showed the decomposition the core just dropped, and a person
                // handed a project built from a design they did not approve has been told the wrong
                // thing about their own project. The wording is the core's, shown verbatim, because
                // the rule is the core's.
                if let warning = bridge.scaffoldSpec?.designWarning {
                    designWarning = warning
                    appendLogLine(
                        "This project already states its design in composition.spire — the design "
                            + "just reviewed was not applied",
                        kind: .phase
                    )
                }
            }

            // The **fill**: the model writes the composition into the tree that now exists, told the
            // decomposition, the framework and the components' own headers. Skipped when there is no
            // design to fill from — an application scaffolded without one is a blank `app_main` on
            // purpose, and filling it would be inventing the design the wizard did not ask for.
            if spec != nil, let scaffold = bridge.scaffoldSpec {
                let fillStarted = await MainActor.run { open("Filling the composition…") }
                guard let plan = await bridge.fillProject(
                    goal: goal,
                    root: root,
                    spec: scaffold,
                    libraryRoot: libraryRoot
                ) else {
                    // The tree exists and is valid; what is missing is the composition. Saying so and
                    // *staying* is the honest end: the project is on disk, opening it as if it were
                    // finished would hide the one fact worth knowing.
                    await MainActor.run {
                        progress = nil
                        finish(
                            "The fill returned no plan — the composition is unwritten",
                            since: fillStarted,
                            kind: .failed
                        )
                        failure = "The tree is scaffolded, but the fill returned no plan — its composition "
                            + "is unwritten. The project is on disk; open it when the model is available."
                        isCreating = false
                    }
                    return
                }
                // The plan's own `Build` step is the one step the sheet does not take from the model.
                // The build directory does not exist until the build at the end of `create` makes it,
                // so running the step here fails on a tree that is otherwise perfect — a `✗` in the
                // log for work that has not happened yet. It is the same build, run later, on
                // something to run it on.
                let (steps, deferredBuilds) = Self.writableSteps(from: plan.steps)
                await MainActor.run {
                    finish("Filled: \(plan.steps.count) step(s) planned", since: fillStarted)
                    if deferredBuilds > 0 {
                        appendLogLine(
                            "Deferring \(deferredBuilds) build step(s) to the build at the end",
                            kind: .phase
                        )
                    }
                }
                let writeStarted = await MainActor.run { open("Writing \(steps.count) step(s)…") }
                let results = await bridge.executeCreationPlan(rootDir: root, steps: steps)
                let written = results.filter { $0.success }.count
                await MainActor.run {
                    finish("\(written) of \(results.count) steps written", since: writeStarted)
                    // One line per step, which is the visible answer to "did it do anything?" — the
                    // question a silent success leaves open.
                    for step in results {
                        appendLogLine(
                            "\(step.success ? "✓" : "✗") \(step.message)",
                            kind: step.success ? .ok : .failed
                        )
                    }
                }

                // **Did the composition land?** The fill is a model's answer, and a model can answer
                // with a *plausible* application instead of the reviewed one — a live run wrote one flat
                // FreeRTOS loop and its own sensor classes. That **compiles**, so nothing else catches
                // it: the check is structural, and a gap stops the pipeline here rather than building,
                // dismissing, and handing over an application nobody designed.
                if let spec {
                    let verifyStarted = await MainActor.run { open("Checking the tree against the design…") }
                    let (gaps, error) = await bridge.verifyComposition(root: root, application: spec)
                    if let error {
                        await MainActor.run {
                            progress = nil
                            finish("Could not check the tree: \(error)", since: verifyStarted, kind: .failed)
                            failure = "The tree was filled, but it could not be checked against the "
                                + "design: \(error)"
                            isCreating = false
                            stage = .review
                        }
                        return
                    }
                    if !gaps.isEmpty {
                        await MainActor.run {
                            progress = nil
                            finish(
                                "Not the reviewed composition: \(gaps.count) gap(s)",
                                since: verifyStarted,
                                kind: .failed
                            )
                            failure = "The tree was filled, but not with the reviewed composition:\n• "
                                + gaps.joined(separator: "\n• ")
                                + "\n\nThe project is on disk. Write `main/` yourself, or take the project "
                                + "somewhere nothing has been written yet and create it again — a second "
                                + "fill over this one would add to it rather than replace it."
                            isCreating = false
                            stage = .review
                        }
                        return
                    }
                    await MainActor.run { finish("Composition matches the design", since: verifyStarted) }
                }
                // **The managed dependencies, settled now that they can be.** The scaffold pinned the
                // board's BSP before the composition existed; a composition that reaches for no board
                // support — a sensor-only app whose display and touch are still stubs — would otherwise
                // build the board's whole peripheral stack and dead-strip every byte of it. The
                // composition is written, so the pin is answered from it, and the first build is the
                // framework and the library rather than that stack.
                if let spec {
                    let depsStarted = await MainActor.run { open("Settling the managed dependencies…") }
                    let (dependencies, error) = await bridge.finalizeApplicationManifest(
                        root: root,
                        application: spec
                    )
                    if let error {
                        await MainActor.run {
                            progress = nil
                            finish(
                                "Could not settle the dependencies: \(error)",
                                since: depsStarted,
                                kind: .failed
                            )
                            failure = "The tree was filled, but its managed dependencies could not be "
                                + "settled: \(error)"
                            isCreating = false
                            stage = .review
                        }
                        return
                    }
                    await MainActor.run {
                        finish(
                            dependencies.isEmpty
                                ? "Managed dependencies: none — nothing beyond the framework is pulled"
                                : "Managed dependencies: " + dependencies.joined(separator: ", "),
                            since: depsStarted
                        )
                        // The note the first build shows is written from *this* answer, not the
                        // scaffold's guess: a build that pinned nothing managed is the framework alone,
                        // and saying "this board's BSP" about it would be wrong.
                        firstBuildNote = Self.firstBuildNote(dependencies: dependencies)
                    }
                }
                // And it is **built**, where this machine can. The loop's product claim is that a
                // designed, applied, scaffolded, filled application builds; a person who is handed a
                // tree nobody compiled has been handed a question, not an application. The verify is
                // the caller's — it owns the toolchain — which is why it happens here and not in the
                // core.
                await buildAndRepair(root: root, spec: scaffold)
            }

            await MainActor.run {
                isCreating = false
                // **A build that failed keeps the sheet.** [`buildAndRepair`] documents that the sheet
                // stays with a project that does not build, and the failure it wrote is the whole point
                // of that — dismissing here discarded the message the instant it was written. That is
                // how a run whose build failed "produced errors" nobody got to read: the tree was on
                // disk, the sheet closed itself, and the one text that named the cause went with it.
                // `failure` is set only on the build's failure paths, so a nil here is a real build.
                guard Self.canCloseAfterBuild(failure: failure) else { return }
                dismiss()
                // A project that was just created is a project the user wants to be in.
                Task { await bridge.openProject(root: root) }
            }
        }
    }

    /// `2 added, 3 already there` — what applying the design did to the library.
    private func addedSummary(_ result: [String: Any]?) -> String {
        let added = (result?["added"] as? [[String: Any]])?.count ?? 0
        let present = (result?["present"] as? [Any])?.count ?? 0
        let errors = (result?["errors"] as? [[String: Any]])?.count ?? 0
        var text = "\(added) added, \(present) already there"
        if errors > 0 { text += ", \(errors) refused" }
        return text
    }

    /// Build the application, and put a failing build **back to the model** — twice at most.
    ///
    /// The build goes through the app's own build path, so it is the same build the dashboard runs,
    /// routed to the same module by the board the design named. The repair is the core's
    /// (`createProject/RepairFromBuild`): the compiler's own lines in, whole-file rewrites out, guarded
    /// by the scaffold's fill roots — and those rewrites are executed by the same `ExecutePlan` every
    /// other step goes through, so the structural guard applies to them too.
    ///
    /// Two passes, because errors cascade: a syntax error hides every type error in the file it broke, so
    /// the first pass fixes what the compiler could see and the second fixes what it could see after
    /// that. A pass that proposes nothing ends the loop, because asking the same question about the same
    /// log is a loop with no exit. What is left over is reported and the sheet **stays** — a project that
    /// does not build is a fact its owner needs, not a detail to discover later.
    private func buildAndRepair(root: String, spec: ScaffoldSpec) async {
        // The platform the **design** names, not the form's picker — see [`buildPlatform`]. A
        // composition opened from a file never filled the form, and no platform sends a
        // `CMakeLists.txt` tree to the CMake module (`cmake --build build`) instead of the ESP-IDF
        // one (`idf.py build`) — the wrong command for a tree that has never been configured.
        let platform = Self.buildPlatform(designChip: design?.board.chip, formChip: board.chip)
        let service = bridge.makeBuildService()
        lastBuildLogAt = nil

        // **The build is watched.** A first build compiles the ESP-IDF framework — and whatever the
        // finalized manifest pinned with it — from source, and a sheet whose only word for those
        // minutes is "Building…" is indistinguishable from one that has hung. This is the same live
        // stream the dashboard's Build panel consumes; it is started here and stopped before this
        // returns, so the dashboard is never left competing for build notifications with a pipeline
        // that has already finished.
        await service.startEventConsumer { lines in
            guard let last = lines.last(where: {
                !$0.line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }) else { return }
            Task { @MainActor in
                let bounded = Self.shortBuildLine(last.line)
                liveBuildLine = bounded
                // The build's own lines go in the log too, thinned: the pinned line above is the
                // unthinned "right now", and this is the sample that leaves the phases legible.
                appendBuildLine(bounded)
            }
        }
        await runBuildPasses(root: root, spec: spec, platform: platform, service: service)
        await service.stopEventConsumer()
        await MainActor.run {
            liveBuildLine = nil
            firstBuild = false
            firstBuildNote = nil
        }
    }

    /// The build/repair loop itself, split from [`buildAndRepair`] so the event consumer that feeds
    /// the live line is started before the first build and stopped after the last. A `defer` cannot
    /// await, and a consumer outliving the pipeline would steal the dashboard's build events.
    private func runBuildPasses(root: String, spec: ScaffoldSpec, platform: String?, service: BuildService) async {
        // The last failed build's own text, kept past the loop so the give-up path can carry it to the
        // log: the sheet's copy of it is capped, and the log is where the whole thing goes.
        var lastBuildOutput = ""
        for pass in 0...2 {
            let started = await MainActor.run {
                // The note about *why* the build is slow is shown only for the build that pays for
                // it; cleared as soon as the build returns.
                firstBuild = pass == 0
                return open(pass == 0 ? "Building…" : "Building after repair \(pass)…")
            }
            let result = try? await service.runTool(
                "build_build",
                path: root,
                language: "cpp",
                platform: platform
            )
            await MainActor.run { firstBuild = false }
            let output = result?.output ?? ""
            // What the toolchain actually said, preferring its output over the runner's summary — the
            // same choice the repair gets a few lines down.
            lastBuildOutput = output.isEmpty ? (result?.error ?? "") : output
            if result?.success == true {
                await MainActor.run {
                    let done = pass == 0 ? "Built ✓" : "Built ✓ after \(pass) repair pass(es)"
                    finish(done, since: started)
                    appendLogLine("built: \(result?.command ?? "build_build")")
                    progress = done
                }
                return
            }

            // The build failed. `output` is the compiler's own text, and the repair wants all of it.
            await MainActor.run { finish("Build failed", since: started, kind: .failed) }
            let repairStarted = await MainActor.run { open("Repairing from the build's errors…") }
            let (repair, error) = await bridge.repairProject(
                root: root,
                diagnostics: output.isEmpty ? (result?.error ?? "") : output,
                spec: spec
            )
            guard let repair else {
                await MainActor.run {
                    finish(
                        "The repair could not run: \(error ?? "unknown")",
                        since: repairStarted,
                        kind: .failed
                    )
                    failBuild(
                        "The project is created but does not build, and the repair could not run: "
                            + (error ?? "unknown")
                            + (BuildEnvironmentHint.forFailure(output: output, error: error)
                                .map { "\n\n\($0)" } ?? ""),
                        output: output
                    )
                }
                return
            }
            await MainActor.run {
                finish("Repair \(pass + 1): \(repair.summary)", since: repairStarted)
            }
            guard repair.hasWork else {
                await MainActor.run {
                    // **A build can fail before the compiler.** A manifest the component manager cannot
                    // resolve produces output with no `error:` line in it, so the repair finds nothing to
                    // rewrite — correctly. The old message ("the compiler's errors are in files this
                    // repair may not rewrite") then named a cause and printed nothing, which is how one
                    // stray comma in a BSP name hid every defect in `main/` behind "nothing to repair".
                    // With no diagnostic to blame, the build's own output is what to show.
                    let why: String
                    if repair.diagnostics == 0 {
                        let said = output.isEmpty ? (result?.error ?? "no output") : output
                        why = "The project is created but does not build, and it failed before the "
                            + "compiler — the build's own output is:\n\n\(said.suffix(2000))"
                    } else {
                        why = "The project is created but does not build — the compiler's errors are in "
                            + "files this repair may not rewrite (the scaffold's own, or the library's). "
                            + repair.unrepaired.prefix(3).joined(separator: "\n")
                    }
                    failBuild(
                        why
                            + (BuildEnvironmentHint.forFailure(output: output, error: result?.error)
                                .map { "\n\n\($0)" } ?? ""),
                        output: output
                    )
                }
                return
            }
            let applied = await bridge.executeCreationPlan(rootDir: root, steps: repair.steps)
            let written = applied.filter { $0.success }.count
            await MainActor.run {
                appendLogLine("Repair \(pass + 1): \(written) of \(applied.count) rewrites written")
            }
        }

        await MainActor.run {
            appendLogLine("Still does not build after two repair passes", kind: .failed)
            failBuild(
                "The project is created but still does not build after two repair passes. Its "
                    + "errors are on the dashboard, where the reviewed fix flow can take them.",
                output: lastBuildOutput
            )
        }
    }

    /// Set the failure the sheet shows **and** write it where it outlives the sheet.
    ///
    /// The two have different lifetimes and both matter: `failure` lives exactly as long as the sheet,
    /// and the log does not. A build that fails on this machine's own environment — no ESP-IDF in it,
    /// so `idf.py` is not there to run — is what makes the difference plain: the message is the only
    /// thing that names the fix, and it is often read *after* the run, out of the log. Every build
    /// failure goes through here, so no path can set the message and skip the record.
    private func failBuild(_ message: String, output: String = "") {
        progress = nil
        failure = message
        isCreating = false
        SpireBridge.logScaffold(Self.buildFailureLogLine(message: message, buildOutput: output))
    }

    /// The scaffold-log entry for a failed build: the sheet's message, then the build's own text.
    ///
    /// Both, because they answer different questions — *what happened*, in the message (with the
    /// environment hint when the failure is the app's own environment), and *what the toolchain
    /// actually said*, in the output, which only some failures carry in their message. Capped,
    /// because a first ESP-IDF build can fail with thousands of lines and this file is read by a
    /// person.
    static func buildFailureLogLine(message: String, buildOutput: String = "") -> String {
        var line = "createProject/Build FAILED: \(message)"
        let output = buildOutput.trimmingCharacters(in: .whitespacesAndNewlines)
        if !output.isEmpty {
            line += "\n  build output (last 4000 chars):\n\(output.suffix(4000))"
        }
        return line
    }

    /// Whether the sheet may close itself once the build/repair loop has finished.
    ///
    /// Only a build that **succeeded** hands the person over to the dashboard. A failure sets
    /// [`failure`], and the contract [`buildAndRepair`] documents is that a project which does not
    /// build stays in front of its owner — closing there is how the one message naming the cause got
    /// thrown away the moment it was written.
    static func canCloseAfterBuild(failure: String?) -> Bool {
        failure == nil
    }

    /// The chip both the scaffold and the build run for: the **design's**, falling back to the form's
    /// picker.
    ///
    /// The design is the authority. On the designed path the form *is* the design, so the two agree;
    /// on the load path — a composition opened from a file — the form was never filled, so
    /// `board.chip` is empty while the design's own `board.chip` carries the file's board. Reading the
    /// form first leaves the build with no platform, and no platform routes a `CMakeLists.txt` tree to
    /// the CMake module (`cmake --build build`) rather than the ESP-IDF one (`idf.py build`) — the
    /// wrong command for a tree that has never been configured, which is how a real ESP-IDF project
    /// failed with "`build` is not a directory".
    ///
    /// `nil` when neither names a chip — a host build, which is what a scaffold without a board means.
    ///
    /// Not `private`: the rule is pure and it is pinned by a test.
    static func buildPlatform(designChip: String?, formChip: String) -> String? {
        let designChip = designChip ?? ""
        let chip = designChip.isEmpty ? formChip : designChip
        return chip.isEmpty ? nil : chip
    }

    /// The one-line record of the platform decision, for `spire-scaffold.log`: both inputs and what
    /// they resolved to.
    ///
    /// It exists because a create run can happen inside a process launched *before* a fix was linked,
    /// and the only thing that tells that apart from a broken rule is the inputs written down. A line
    /// reading `design.chip=nil form.chip=nil -> nil` for a composition whose tree names a chip means
    /// the binary is stale, not that [`buildPlatform`] chose wrong.
    ///
    /// Not `private`: the line is a pure function of the two chips and it is pinned by a test.
    static func buildPlatformLogLine(designChip: String?, formChip: String) -> String {
        let chip = buildPlatform(designChip: designChip, formChip: formChip)
        let platforms = chip.map { [$0] } ?? []
        return "CreateProjectSheet.create() platform: design.chip=\(designChip ?? "nil") "
            + "form.chip=\(formChip.isEmpty ? "nil" : formChip) -> \(chip ?? "nil") "
            + "platforms=\(platforms)"
    }

    /// **The design and the library disagree**, as the sheet states it — or `nil` when they agree.
    ///
    /// `idf_apply_design` reports a component the design marks `existing` that the library does not have
    /// in `problems`, not `error`: it still writes the components it *can* write, and the scaffold that
    /// follows names the missing one in the application's `REQUIRES`. So what would fail is the build, a
    /// minute later, on `Failed to resolve component 'moving_average' required by component 'main'` — a
    /// symptom that names neither the design nor the fix. The core's own words are the actionable half
    /// ("the design says the library already has `moving_average` and it does not — add it … or design
    /// it out"), so they are kept, and the run stops before a file is written.
    ///
    /// Not `private`: the rule is pure and it is pinned by a test.
    static func designLibraryDisagreement(_ problems: [String]) -> String? {
        guard !problems.isEmpty else { return nil }
        return "The reviewed design and the component library disagree:\n• "
            + problems.joined(separator: "\n• ")
            + "\n\nFix the composition or the library and create it again — an application scaffolded "
            + "against a component nobody has cannot build."
    }

    /// Split the fill's plan into the steps the sheet writes and the build steps it defers.
    ///
    /// The model tends to end its plan with a `Build` step, and that is the one step the sheet does
    /// not take from it: the build directory does not exist until the build at the end of `create`
    /// makes it, so running the step early fails on a tree that is otherwise perfect — a `✗` in the
    /// log for work that has not happened yet. The deferred count lets the log say so rather than
    /// dropping a step in silence.
    ///
    /// Not `private`: the rule is pure and it is pinned by a test.
    static func writableSteps(from steps: [CreationStep]) -> (writes: [CreationStep], deferredBuilds: Int) {
        let writes = steps.filter { $0.stepType != .build }
        return (writes, steps.count - writes.count)
    }

    /// One build line, bounded to something a caption can hold. Compiler lines are long — a
    /// managed-component path, an error, and the code context — and the caption shows the tail.
    /// Bounding it is about not holding a megabyte of log in view state, not about what is shown.
    /// Not `private`: the bound is a pure rule of the string, and it is pinned by a test.
    static func shortBuildLine(_ line: String) -> String {
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.count <= 160 ? trimmed : "…" + String(trimmed.suffix(159))
    }

    /// `0.4s`, `12s`, `1m 15s` — how long a phase took.
    ///
    /// Sub-ten-second phases keep a decimal, because the phases worth telling apart here are a *fast*
    /// one (the scaffold) and a slow one (a build), and `0s` for a scaffold that did happen reads as
    /// "nothing happened". Nothing is rounded down to zero: the smallest answer is `0.0s`.
    static func durationText(_ seconds: TimeInterval) -> String {
        let whole = Int(max(0, seconds).rounded())
        if whole < 10 { return String(format: "%.1fs", max(0, seconds)) }
        if whole < 60 { return "\(whole)s" }
        let minutes = whole / 60
        let remainder = whole % 60
        return remainder == 0 ? "\(minutes)m" : "\(minutes)m \(remainder)s"
    }

    /// `14:09:37` — the wall-clock stamp a log line carries.
    ///
    /// Fixed and locale-independent, because a log is read next to other logs (and next to the file
    /// log), and built per call rather than from a shared formatter, because a creation writes a
    /// handful of lines and a shared `DateFormatter` is not safe to use from several actors.
    static func logStamp(_ date: Date, timeZone: TimeZone = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "HH:mm:ss"
        formatter.timeZone = timeZone
        return formatter.string(from: date)
    }

    /// Why the first build is slow, said in terms of what the finalized manifest actually pinned.
    ///
    /// It used to be one sentence about "the ESP-IDF framework and this board's BSP", which was true
    /// of the scaffold's guess and is not true of every composition: a sensor-only application whose
    /// display and touch are still stubs pins **nothing** managed once the manifest is settled, and
    /// its first build is the framework alone. Naming a BSP that build never pulled is the kind of
    /// note a person stops believing. The dependency list is the answer, so the note is a function of
    /// it — and a pure one, so it is pinned by a test.
    static func firstBuildNote(dependencies: [String]) -> String {
        if dependencies.isEmpty {
            return "A first build compiles the ESP-IDF framework from source, so it can take a minute "
                + "or two. Later builds reuse that work and finish in seconds."
        }
        return "A first build compiles the ESP-IDF framework and the \(dependencies.count) managed "
            + "component\(dependencies.count == 1 ? "" : "s") this composition names — a board's BSP "
            + "brings its whole peripheral stack with it — from source, so it can take a few minutes. "
            + "Later builds reuse that work and finish in seconds."
    }
}
