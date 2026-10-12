import SwiftUI

/// The experimental **board-and-resources** flow — v1 of the new UI, reachable only behind the
/// `experimentalFlow` flag so the existing wizard is untouched.
///
/// Four steps, each the minimum that makes the next one honest:
///
/// 1. **Describe** — the application in the person's own words, read by the model into a **design
///    brief**: the capabilities it needs, the framework it points at, the units it expects and the
///    description read against the **six points** — what the text said, and what was decided where it
///    said nothing. The one or two decisions the model will not make alone come back as **picks**, and
///    picking one edits the draft the later steps read. The brief is a **guide, not a gate** — it badges
///    the board's fit and the resources the application is still missing, and it never blocks Continue.
/// 2. **Board** — the boards the core knows, filtered to what a firmware application is built *for*.
///    Choosing one reads its resolved profile (`capabilities/profile`), because a board's parts are
///    facts the graph already holds and Swift must not re-derive them.
/// 3. **Resources** — one table, one row per **source**: every part the chosen board declares, then
///    every category of the capability vocabulary (`capabilities/vocabulary`). A row's first column is
///    the source's name; its second is the resources that source offers — **empty** until something is
///    chosen from it, then it keeps showing the selections, so a chosen resource stays visible even
///    after the eye moves to another row. Choosing a **part** row is a choice — the part *is* the
///    resource it offers, so the row's own click puts it in and shows the resource selected and
///    read-only; choosing a **category** row opens it, revealing its paths to tick (and, once closed,
///    keeping the ticked ones in view), because a category holds many and the application says which.
///    Both feed one `resources:` set.
/// 4. **Actors** — the resources just declared, each owned by exactly one actor, and the actors
///    themselves: the message each receives, what it holds, and who it sends to. Ownership and
///    "sends to" are the whole of the composition here, and the step mirrors the core's rules live —
///    an unowned resource, a bad or shadowing id, a missing message or a wired-to-nobody actor
///    **disables** Continue rather than being discovered as a refusal.
///
/// Continue on the last step produces a **spec** — an `actors` composition with the driver components
/// the `library` resources imply — and shows it for review through the reader the wizard already has.
/// Handing it to the scaffold is the follow-up; this step's job is to write something that would
/// validate.
struct BoardResourceFlowView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    @State private var state = BoardResourceFlowState()
    @State private var step: Step = .describe
    @State private var isLoading = true
    @State private var isChoosing = false
    /// The describe step's own busy flag and refusal — the model call is the core's, so all the view
    /// does is hold the button down while it is out and show the answer when it is back.
    @State private var isBriefing = false
    @State private var briefError: String?
    /// The actor-assignment step's state — the resources and who owns them. Held apart from `state`
    /// because it speaks about a later phase, and synced from `state` whenever the step is entered.
    @State private var assignment = ActorAssignmentState()
    /// Whether the last step is showing the assembled spec rather than the editor.
    @State private var showReview = false
    /// Which actor rows are **open** in the assignment step — the view state that lets the pane read as
    /// a list of names and summaries, with the editable fields revealed in place. Ephemeral: it is where
    /// the eye is, not part of the draft, so it lives in the view rather than the model.
    @State private var expandedActors: Set<UUID> = []
    /// Which table row is open — a board part or a catalogue category. Ephemeral: it is where the eye
    /// is, not part of the draft, so it lives in the view. An open **part** row is one whose part is in
    /// the draft; a **category** row is open until it is closed. Closing a row does not hide what was
    /// chosen from it — the second column keeps the selections in view.
    @State private var selectedSource: Source?

    private enum Step { case describe, board, resources, actors }


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
        case .describe: describeStep
        case .board: boardStep
        case .resources: resourcesStep
        case .actors: actorsStep
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
                Text(headerSubtitle)
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
            }
            Spacer()
            Text("\(stepNumber) of 4")
                .font(.caption)
                .foregroundStyle(theme.textTertiary)
        }
    }

    /// The subtitle under the title — what this step is for, in one line.
    private var headerSubtitle: String {
        switch step {
        case .describe: return "Say what you want to build, in your own words"
        case .board: return "Choose the board this application is built for"
        case .resources: return "The devices this application composes against"
        case .actors: return "The application's actors, and what each one does"
        }
    }

    /// The step's ordinal, for "1 of 4".
    private var stepNumber: Int {
        switch step {
        case .describe: return 1
        case .board: return 2
        case .resources: return 3
        case .actors: return 4
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if step != .describe {
                Button("Back") { goBack() }
                if step == .resources {
                    Text("\(state.resources.count) selected")
                        .font(.caption)
                        .foregroundStyle(theme.textTertiary)
                } else if step == .actors && !showReview {
                    Text("\(assignment.owners.count) of \(assignment.resources.count) owned")
                        .font(.caption)
                        .foregroundStyle(theme.textTertiary)
                }
            }
            Spacer()
            Button("Close") { dismiss() }
            switch step {
            case .describe:
                // A description is the one thing this step cannot go on without; the brief is optional,
                // so a person may move to the board with nothing summarized — the later steps simply
                // lose the steer.
                Button("Continue") { step = .board }
                    .disabled(!state.canLeaveDescribe)
            case .board:
                EmptyView()
            case .resources:
                Button("Continue") { startAssigning() }
                    .disabled(!state.canContinue)
            case .actors:
                if showReview {
                    Button("Edit") { showReview = false }
                } else {
                    Button("Continue") { showReview = true }
                        .disabled(!assignment.canContinue)
                }
            }
        }
    }

    /// Back one step. The actors the person already wrote are **kept** — the assignment is synced
    /// from the declared resources when the step is entered, never rebuilt — and a review is left
    /// before the editor it covers.
    private func goBack() {
        showReview = false
        switch step {
        case .describe: break
        case .board: step = .describe
        case .resources: step = .board
        case .actors: step = .resources
        }
    }

    /// Leave the resources step for the actors step: sync the assignment with what was declared (the
    /// actors already written are kept), start from the brief's suggested units, and clear any stale
    /// review.
    private func startAssigning() {
        guard state.canContinue else { return }
        assignment.sync(
            board: state.selectedBoard, resources: state.resources, description: state.description)
        // The brief's unit suggestions seed the actors — additive and idempotent, so coming back here
        // never duplicates them and never touches the rows the person wrote.
        assignment.suggestActors(state.resolvedBrief?.suggests ?? [])
        showReview = false
        step = .actors
    }


    // MARK: - Step 1: describe

    /// The first step: the application in the person's own words, and the **brief** the model reads
    /// out of it.
    ///
    /// Free text rather than six questions, deliberately: description-first means the person writes what
    /// they want and the model does the structuring. The six questions are still in the *request*, where
    /// the model answers each one — deciding it where the description is silent — instead of handing the
    /// person a form to fill or a list of questions to answer in prose. The card shows that reading back
    /// as six labelled rows, and the decisions the model will not make alone as picks.
    ///
    /// "Summarize" is optional and repeatable: the brief is a **guide, not a gate**, so a person may go
    /// on without one, and edit the description and re-read it as many times as they like.
    private var describeStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            SectionHeading("Describe")
            Text(
                "Say what you want the application to do, in your own words. Summarize reads it into "
                    + "a brief — the capabilities it needs, the framework it points at and the units "
                    + "it implies — which guides the board and resources steps. Summarize is "
                    + "repeatable: edit the text and read it again until the open questions are the "
                    + "ones you meant to leave open."
            )
            .font(.caption)
            .foregroundStyle(theme.textSecondary)
            .fixedSize(horizontal: false, vertical: true)

            TextEditor(text: $state.description)
                .font(.callout)
                .frame(minHeight: 110)
                .padding(6)
                .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.divider, lineWidth: 1))

            HStack(spacing: 10) {
                Button("Summarize") { Task { await summarize() } }
                    .disabled(!state.canLeaveDescribe || isBriefing)
                if isBriefing { ProgressView().controlSize(.small) }
                Spacer(minLength: 0)
            }

            if let briefError {
                Text(briefError)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let brief = state.resolvedBrief {
                ScrollView { briefCard(brief) }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else {
                Spacer(minLength: 0)
            }
        }
    }

    /// Read the description into a brief. The model call is the core's; here we hold the busy flag, put
    /// the brief where the later steps read it, and show the refusal when there is one — the step is
    /// never thrown away by a guide.
    private func summarize() async {
        guard !isBriefing, state.canLeaveDescribe else { return }
        isBriefing = true
        briefError = nil
        let (brief, error) = await bridge.briefApplication(description: state.description)
        if let brief {
            // A fresh reading answers none of the old forks: the picks are dropped with the brief they
            // belonged to, so an answer can never land on a decision that is no longer on screen.
            state.clearForkPicks()
            state.brief = brief
        } else {
            briefError = error ?? "the brief came back empty"
        }
        isBriefing = false
    }

    /// The brief, shown for confirmation: the summary the model read back, the framework it derived,
    /// the capabilities the application needs, the units it suggests, **the description read against the
    /// six points**, and the one or two decisions it left to the person. Everything here is a **guide** —
    /// the framework is confirmed when a board is chosen, and `requires` drives badges later rather than
    /// a gate now.
    private func briefCard(_ brief: DesignBrief) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                SectionHeading("Brief")
                Spacer(minLength: 4)
                if let framework = brief.framework {
                    badge(framework, tint: theme.accent)
                }
            }
            if !brief.summary.isEmpty {
                Text(brief.summary)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !brief.requires.isEmpty {
                briefField("Needs", brief.requiresLabel)
            }
            if !brief.suggests.isEmpty {
                briefField("Units", brief.suggests.joined(separator: ", "))
            }
            pointsField(brief)
            forksField(brief)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
    }

    /// One labelled line of the brief card — a small caps label over its value.
    private func briefField(_ label: String, _ value: String, tint: Color? = nil) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label).font(.caption2.weight(.semibold)).foregroundStyle(theme.textTertiary)
            Text(value)
                .font(.caption)
                .foregroundStyle(tint ?? theme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The brief's **six points** — the description read against each — drawn as six labelled rows.
    ///
    /// Six always, iterated from `BriefPoint.allCases` rather than from what the model returned, so the
    /// list **cannot get shorter between two reads of the same text**. A point the model skipped is a row
    /// whose reading is empty, which names exactly which question went unanswered; the numbered list this
    /// replaced simply had fewer lines and said nothing about what had been dropped.
    ///
    /// There is deliberately **no count** anywhere in the label. How many points a description leaves open
    /// is the judgement that moved between live runs — 0, then 1, then 6 for one unchanged text — so it is
    /// not shown as a number at all. What is open is legible row by row, which is a fact about the text
    /// rather than a score about the read.
    @ViewBuilder
    private func pointsField(_ brief: DesignBrief) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text("Read against the six points")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(theme.textTertiary)
            ForEach(BriefPoint.allCases) { point in
                pointRow(point, read: brief.reading(of: point))
            }
        }
    }

    /// One point: its label, what the description **said** about it, and — where the description said
    /// nothing — the **decision the model took instead of asking**, marked as the model's own so a person
    /// can tell their text from its draft.
    private func pointRow(_ point: BriefPoint, read: BriefPointRead?) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(point.label)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(theme.textTertiary)
            if let read, !read.isBlank {
                if !read.said.isEmpty {
                    Text(read.said)
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if !read.decided.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: 4) {
                        Image(systemName: "pencil")
                            .font(.system(size: 9))
                        Text(read.decided)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .font(.caption)
                    .foregroundStyle(theme.accent)
                }
            } else {
                // Not a gap and not a question: a row the model did not read, shown so the person knows
                // which point went unread rather than finding a shorter list.
                Text("not read")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// The brief's **forks** — the one or two decisions it would not make for the person — as **picks**.
    ///
    /// Each is a question with its alternatives as buttons, and picking one merges the capabilities that
    /// answer carries into the brief the later steps read (`state.resolvedBrief`), so answering is an edit
    /// to the draft rather than a sentence the model would have to read again. Picking the same choice
    /// twice takes the answer back, and the merge is recomputed from the model's own brief each time, so
    /// nothing accumulates.
    ///
    /// Nothing is drawn when there are no forks — no count and no "nothing to decide" line, because that
    /// would be a claim about the reading rather than a thing the person can act on. The six rows above
    /// already say what was read and what was decided.
    @ViewBuilder
    private func forksField(_ brief: DesignBrief) -> some View {
        if !brief.forks.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("Your call")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(theme.textTertiary)
                ForEach(Array(brief.forks.enumerated()), id: \.offset) { index, fork in
                    forkRow(index, fork)
                }
            }
        }
    }

    /// One fork: the point it belongs to, the decision, its alternatives as buttons, and what the picked
    /// answer adds.
    private func forkRow(_ index: Int, _ fork: BriefFork) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            if let point = fork.briefPoint {
                Text(point.label)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(theme.textTertiary)
            }
            Text(fork.question)
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 6) {
                ForEach(Array(fork.choices.enumerated()), id: \.offset) { choiceIndex, choice in
                    choiceButton(index, choiceIndex, choice)
                }
            }
            if let picked = state.forkPicks[index], fork.choices.indices.contains(picked) {
                Text("adds \(fork.choices[picked].additionLabel)")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// One alternative, drawn as a capsule that fills in when it is the answer. The same shape as
    /// [`badge`], filled rather than tinted, because here the person is looking at *what is chosen* rather
    /// than at what is worth noticing.
    private func choiceButton(_ fork: Int, _ choice: Int, _ alternative: BriefChoice) -> some View {
        let picked = state.forkPicks[fork] == choice
        return Button {
            state.toggleFork(fork, choice)
        } label: {
            Text(alternative.label)
                .font(.caption.weight(picked ? .semibold : .regular))
                .foregroundStyle(picked ? Color.white : theme.accent)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .background(
                    Capsule().fill(picked ? theme.accent : theme.accent.opacity(0.12))
                )
        }
        .buttonStyle(.plain)
        .help(alternative.additionLabel.isEmpty ? "" : "adds \(alternative.additionLabel)")
    }

    /// A small capsule badge — the flow's one shape for "this is worth noticing" (`Needed`, the
    /// framework's name). Inlined rather than a shared component: it is a few lines and this flow is the
    /// only place that draws it.
    private func badge(_ text: String, tint: Color) -> some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .foregroundStyle(tint)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(Capsule().fill(tint.opacity(0.15)))
    }

    // MARK: - Step 2: board

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

    /// The brief's steer for this step: what the application needs, how much of it this board carries
    /// as parts, and what is still uncovered. A **guide** — it never disables Continue; it tells the
    /// person where to look.
    @ViewBuilder
    private var briefBanner: some View {
        if let brief = state.resolvedBrief, !brief.isEmpty {
            let needs = brief.requires
            let onBoard = needs.count - state.requirementsNotOnBoard(brief).count
            let uncovered = state.uncoveredRequirements(brief)
            let capabilityWord = needs.count == 1 ? "capability" : "capabilities"
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Image(systemName: "text.magnifyingglass")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.accent)
                    Text("The brief asks for \(needs.count) \(capabilityWord)")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(theme.textPrimary)
                    Spacer(minLength: 4)
                    if let framework = brief.framework {
                        badge(framework, tint: theme.accent)
                    }
                }
                Text("This board carries \(onBoard) of them as parts — add the rest below.")
                    .font(.caption2)
                    .foregroundStyle(theme.textSecondary)
                if !uncovered.isEmpty {
                    Text("Not yet covered: \(uncovered.joined(separator: ", "))")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        }
    }

    // MARK: - Step 3: resources

    private var resourcesStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            if let board = state.selectedBoard {
                Text(board.name).font(.title3.weight(.semibold))
            }
            briefBanner

            // One table, one row per source: a part the board carries, or a catalogue category.
            // The first column is the name; the second is that source's resources — empty until
            // something is chosen from it, then kept in view so a selection is never lost when the
            // eye moves on. A part row is a choice (one part is one resource, so choosing it puts
            // the part in); a category row opens to show its paths to tick, and once closed keeps
            // the ticked ones visible, because a category holds many paths and the application
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


    /// The table: every part the board declares, then every category of the capability vocabulary,
    /// on one list. Each row is the same two columns — the source's name, then the resources that
    /// source offers — so a part and a category read the same way. The hint sits above it only while
    /// the table is truly untouched — nothing open *and* nothing chosen — because a chosen resource is
    /// itself kept in view.
    private var sourceTable: some View {
        VStack(alignment: .leading, spacing: 0) {
            if selectedSource == nil && state.resources.isEmpty {
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
    /// **resources** of the source: open shows everything it offers, and a closed row keeps showing the
    /// resources already chosen from it, so a selection is continuously visible rather than vanishing
    /// when the eye moves to another row.
    private func sourceRow(_ source: Source, title: String, note: String) -> some View {
        let open = selectedSource == source
        return HStack(alignment: .top, spacing: 0) {
            Button { choose(source) } label: {
                nameCell(source, title: title, note: note, open: open)
            }
            .buttonStyle(.plain)
            .frame(width: 196, alignment: .leading)

            Divider().overlay(theme.divider)

            resourceCell(source, open: open)
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

    /// The second column of a row: the resources the source offers — **open** shows everything it
    /// offers (a part's one read-only resource; a category's tickable paths), and a **closed** row
    /// keeps showing what was already chosen from it, so a selection stays continuously visible rather
    /// than disappearing when the eye moves to another row. A row with nothing chosen stays empty.
    @ViewBuilder
    private func resourceCell(_ source: Source, open: Bool) -> some View {
        switch source {
        case .part(let name):
            if state.isBoardPartSelected(name),
                let device = state.profile?.devices.first(where: { $0.name == name }) {
                partResource(device)
            }
        case .category(let name):
            if let category = state.catalogue.first(where: { $0.category == name }) {
                if open {
                    categoryResources(category)
                } else {
                    categorySelections(category)
                }
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
            if device.roles.contains(where: { state.isRequired($0) }) {
                badge("Needed", tint: theme.accent)
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

    /// A category's **chosen** resources, shown read-only while the row is closed: just the additions
    /// the application ticked, so a selection made in a category stays visible after it is closed. A
    /// `via: board` resource is left out — it lives in the board's own part row, its one control — so
    /// the two sections never both claim the same capability.
    private func categorySelections(_ category: VocabularyCategory) -> some View {
        let chosen = state.resources.filter {
            $0.via != .board && $0.roles.contains { category.capabilities.contains($0) }
        }
        return VStack(alignment: .leading, spacing: 2) {
            ForEach(chosen) { draft in
                selectedResource(draft)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// One chosen resource as a read-only line — a check-mark, its name and roles, and how it is
    /// reached — the same shape a part's resource takes, so the two read alike.
    private func selectedResource(_ draft: ResourceDraft) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 12))
                .foregroundStyle(theme.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text(draft.name).font(.callout)
                Text(draft.rolesLabel)
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            Text(draft.via.label)
                .font(.caption2)
                .foregroundStyle(theme.textTertiary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    /// The one-line summary a part carries in the first column: its part number when the board named
    /// one, else its roles — short, because the row also shows the name.
    private func summary(for device: BoardDevice) -> String {
        if !device.part.isEmpty { return device.part }
        return device.rolesLabel
    }

    /// The same line for a part the board declares — its roles, the part number it carries, and what
    /// it is reached on, read from the board's own `devices:` block.
    private func detail(for device: BoardDevice) -> String {
        var parts = [device.rolesLabel]
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
                if state.isRequired(path) {
                    badge("Needed", tint: theme.accent)
                }
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

    // MARK: - Step 4: actors

    /// The assignment editor, or the assembled spec for review once Continue is pressed.
    @ViewBuilder
    private var actorsStep: some View {
        if showReview {
            reviewStep
        } else {
            assignmentEditor
        }
    }

    /// One list, **actors first**: each actor reads as a line — its name and what it does — and opens in
    /// place to the message, state, owned devices and "sends to" the spec is assembled from. The
    /// resources follow, each owned by exactly one actor, because ownership is a *detail* of an actor's
    /// line rather than the first thing the person has to read. A resource is moved between actors by
    /// picking a different owner — one device, one owner — and the "sends to" chips are the edges the
    /// `wiring` is rendered from.
    private var assignmentEditor: some View {
        VStack(alignment: .leading, spacing: 10) {
            briefHint
            if assignment.resources.isEmpty {
                Text("Nothing to assign — go back and choose at least one device.")
                    .font(.callout)
                    .foregroundStyle(theme.textTertiary)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        actorList
                        resourceOwnerPane
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            problemsList
        }
    }

    /// What the brief said, carried into this step: the summary as one line, the framework as a badge,
    /// and — when the brief pointed at `ramen` — a soft note that this flow composes `actors`. The
    /// **suggested units** are not repeated here: they are the actor rows themselves, added by
    /// [`startAssigning`](BoardResourceFlowView.startAssigning).
    ///
    /// A **guide**: the framework is the brief's derivation, and this flow's assembled spec is always an
    /// `actors` composition — so a `ramen` brief is reported rather than silently ignored.
    @ViewBuilder
    private var briefHint: some View {
        if let brief = state.resolvedBrief, !brief.isEmpty, !brief.summary.isEmpty {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Image(systemName: "text.magnifyingglass")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.accent)
                    Text(brief.summary)
                        .font(.caption)
                        .foregroundStyle(theme.textPrimary)
                        .lineLimit(2)
                    Spacer(minLength: 4)
                    if let framework = brief.framework {
                        badge(framework, tint: theme.accent)
                    }
                }
                if brief.framework == "ramen" {
                    Text(
                        "The brief points at a `ramen` pipeline; this flow composes `actors`. Say it in "
                            + "the description if that matters, or build it from the framework wizard."
                    )
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        }
    }

    // MARK: resources and their one owner

    /// The declared resources, each with its **one** owner. A `library` resource also states its
    /// driver's **bus** here, because the driver only exists once the spec is assembled — and a driver
    /// with no bus never compiles (rule 4).
    private var resourceOwnerPane: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                SectionHeading("Resources (\(assignment.resources.count))")
                Spacer(minLength: 4)
                // The rule-8 failure, said where the resources are: a device no actor owns is one the
                // decomposition has left out — the gap a person checking it is looking for.
                if !assignment.unownedResources.isEmpty {
                    Text("\(assignment.unownedResources.count) unowned")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                ForEach($assignment.resources) { $resource in
                    resourceOwnerRow($resource)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func resourceOwnerRow(_ resource: Binding<ResourceDraft>) -> some View {
        let draft = resource.wrappedValue
        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(draft.name).font(.callout.weight(.medium))
                Text(draft.via.label)
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
                Spacer(minLength: 4)
                ownerPicker(draft.name)
            }
            Text(draft.rolesLabel)
                .font(.caption2)
                .foregroundStyle(theme.textSecondary)
            if draft.via != .board {
                busField(bus: resource.bus)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
    }

    /// The driver's **bus** — the protocol it speaks (`i2c`, `spi`), which the resource's wiring
    /// function is not. Required for a `library` resource, because the driver written from it states
    /// its bus (rule 4) — so it arrives pre-filled with [`defaultDriverBus`] and the person confirms
    /// or changes it here.
    private func busField(bus: Binding<String>) -> some View {
        HStack(spacing: 6) {
            Text("driver bus").font(.caption2).foregroundStyle(theme.textSecondary)
            TextField("i2c", text: bus)
                .textFieldStyle(.roundedBorder)
                .font(.caption)
                .frame(width: 90)
        }
    }

    /// The resource's **owner**: one actor, or none. Picking another **moves** it — one device, one
    /// owner — which is what keeps rule 8 honest without a "shared" state to warn about.
    private func ownerPicker(_ resource: String) -> some View {
        Picker("", selection: ownerBinding(resource)) {
            Text("unowned").tag(UUID?.none)
            ForEach(assignment.actors, id: \.uid) { actor in
                Text(actor.id.isEmpty ? "(unnamed)" : actor.id).tag(Optional(actor.uid))
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .controlSize(.small)
        .fixedSize()
        .disabled(assignment.actors.isEmpty)
    }

    /// The picker's binding: reading the stored owner, writing through `assign` so the move is one
    /// gesture and a resource can never end up with two owners.
    private func ownerBinding(_ resource: String) -> Binding<UUID?> {
        Binding(
            get: { assignment.ownerUID(of: resource) },
            set: { assignment.assign(resource, to: $0) }
        )
    }

    // MARK: the actors

    /// The actors, as a **list of what each one is**: the name on the first line, and under it the
    /// derived [`summary`](ActorAssignmentState.summary(of:)) — the message, state, owned devices and
    /// peers as one sentence — so the decomposition can be read and checked at a glance. A row opens in
    /// place to the fields the summary is derived from. "Add actor" appends an empty row and opens it —
    /// the person names it, because a name is a decision.
    private var actorList: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                SectionHeading("Actors (\(assignment.actors.count))")
                Spacer()
                Button("Add actor") { openNewActor() }
                    .controlSize(.small)
            }
            if assignment.actors.isEmpty {
                Text("No actors yet — add one to own the devices below.")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach($assignment.actors, id: \.uid) { $actor in
                        actorCard($actor)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// Add an actor and open its row, so its id is there to type rather than a collapsed line to hunt
    /// for.
    private func openNewActor() {
        expandedActors.insert(assignment.addActor())
    }

    /// One actor, as a **line to read and a row to open**: the name and the derived summary are always
    /// in view — so the list says what each actor *is* — and the editable fields are revealed in place
    /// when the row is opened. The summary is what makes the decomposition checkable: a glance down the
    /// list says whether anything owns the display or the touch.
    private func actorCard(_ actor: Binding<ActorDraft>) -> some View {
        let draft = actor.wrappedValue
        let open = expandedActors.contains(draft.uid)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Button { toggleActor(draft.uid) } label: {
                    HStack(spacing: 6) {
                        Image(systemName: open ? "chevron.down" : "chevron.right")
                            .font(.system(size: 9, weight: .semibold))
                            .foregroundStyle(theme.textTertiary)
                        Text(draft.id.isEmpty ? "(unnamed)" : draft.id)
                            .font(.callout.weight(.medium))
                            .foregroundStyle(theme.textPrimary)
                    }
                }
                .buttonStyle(.plain)
                Spacer(minLength: 4)
                Button { assignment.removeActor(draft.uid) } label: {
                    Image(systemName: "trash").font(.system(size: 11))
                }
                .buttonStyle(.plain)
                .foregroundStyle(theme.textTertiary)
                .help("Remove this actor")
            }
            // The summary — what the actor *is*, derived from the fields below — so the pane is read as
            // a list of what each actor does rather than a wall of inputs.
            Text(assignment.summary(of: draft.uid))
                .font(.caption)
                .foregroundStyle(theme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            if open {
                Divider()
                actorEditor(actor)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
    }

    /// Open or close one actor's row — the view state that makes the pane a **list** rather than a form.
    private func toggleActor(_ uid: UUID) {
        if expandedActors.contains(uid) {
            expandedActors.remove(uid)
        } else {
            expandedActors.insert(uid)
        }
    }

    /// The opened actor: the fields the summary is derived from — the id it is referred to by, the
    /// message it receives, what it holds, the resources it owns (read-only here — a resource is claimed
    /// on its own row) and who it sends to.
    private func actorEditor(_ actor: Binding<ActorDraft>) -> some View {
        let draft = actor.wrappedValue
        let owned = assignment.resourcesOwned(by: draft.uid)
        return VStack(alignment: .leading, spacing: 6) {
            labelledField("id (lower_snake)", text: actor.id, placeholder: "sampler")
            HStack(spacing: 6) {
                labelledField("message", text: actor.message, placeholder: "SensorMsg")
                labelledField("state", text: actor.state, placeholder: "the last reading")
            }
            labelledField(
                "does (heads its doc-comment)", text: actor.description,
                placeholder: "wakes on the timer and reads the sensor")
            if !owned.isEmpty {
                HStack(spacing: 4) {
                    Text("owns").font(.caption2).foregroundStyle(theme.textSecondary)
                    ForEach(owned, id: \.self) { ownerChip($0) }
                }
            }
            sendToRow(draft)
        }
    }

    private func labelledField(
        _ label: String, text: Binding<String>, placeholder: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label).font(.caption2).foregroundStyle(theme.textSecondary)
            TextField(placeholder, text: text)
                .textFieldStyle(.roundedBorder)
                .font(.caption)
        }
    }

    /// A resource the actor owns, with the one gesture that gives it back — it becomes **unowned**,
    /// never silently another actor's.
    private func ownerChip(_ name: String) -> some View {
        HStack(spacing: 3) {
            Text(name).font(.caption2).foregroundStyle(theme.textPrimary)
            Button { assignment.assign(name, to: nil) } label: {
                Image(systemName: "xmark").font(.system(size: 7, weight: .bold))
            }
            .buttonStyle(.plain)
            .foregroundStyle(theme.textTertiary)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Capsule().fill(theme.accentBackground))
    }

    /// Who this actor **sends to** — the one place the edges are stated, rendered into the `wiring`
    /// and into each unit's `sends_to`. Only other actors are offered: an actor posts to an actor.
    @ViewBuilder
    private func sendToRow(_ actor: ActorDraft) -> some View {
        let peers = assignment.actors.filter { $0.uid != actor.uid }
        HStack(alignment: .top, spacing: 4) {
            Text("sends to").font(.caption2).foregroundStyle(theme.textSecondary)
            if peers.isEmpty {
                Text("— add another actor to wire this one")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            } else {
                HStack(spacing: 4) {
                    ForEach(peers, id: \.uid) { peer in
                        peerChip(actor, peer)
                    }
                }
            }
        }
    }

    /// One peer as a toggle: on means an edge from this actor, which is also one entry in its
    /// `sends_to`.
    private func peerChip(_ actor: ActorDraft, _ peer: ActorDraft) -> some View {
        let on = actor.sendsTo.contains(peer.uid)
        return Button {
            assignment.toggleSend(from: actor.uid, to: peer.uid)
        } label: {
            Text(peer.id.isEmpty ? "(unnamed)" : peer.id)
                .font(.caption2)
                .foregroundStyle(on ? theme.accent : theme.textSecondary)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(Capsule().fill(on ? theme.accentBackground : theme.surface))
                .overlay(Capsule().stroke(on ? theme.accent : theme.divider, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    /// Everything the core would refuse, said while the person can still fix it — the button is
    /// **disabled**, not the spec refused.
    @ViewBuilder
    private var problemsList: some View {
        let problems = assignment.problems
        if !problems.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(problems, id: \.self) { problem in
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(.orange)
                        Text(problem)
                            .font(.caption2)
                            .foregroundStyle(theme.textSecondary)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: the review

    /// The assembled spec, shown **for approval**: the same read-only review the design phase uses,
    /// decoded through the model that already exists — so what is read here is the spec that would
    /// validate, and the record itself is there for anyone who wants to read it.
    @ViewBuilder
    private var reviewStep: some View {
        if let design = assignment.design {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    SectionHeading("Review")
                    Text("This is the application the assignment produces. Nothing is written yet.")
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                    DesignReviewView(design: design)
                    specSection
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            Text("The spec could not be assembled — go back and fix what is listed.")
                .font(.callout)
                .foregroundStyle(theme.textTertiary)
                .textSelection(.enabled)
        }
    }

    private var specSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionHeading("Spec")
            ScrollView {
                Text(assignment.specText)
                    .font(.system(.caption2, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            }
            .frame(height: 220)
        }
    }

}

#Preview {
    BoardResourceFlowView()
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .frame(width: 720, height: 600)
}

