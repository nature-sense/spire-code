import SwiftUI

/// The one sheet a component needs: what it **is**, and what the user knows about it.
///
/// Add and edit are the same sheet because they end in the same place — a description in the user's
/// own words — and differ by two fields at the top. What the description is *for* depends on the kind:
/// a driver's comes off a datasheet (command words, register addresses, checksums), a library's is what
/// the code should do and with what inputs and outputs. The box says which, rather than inviting the
/// wrong one.
///
/// The **chip** picker is here for both kinds, because the second gate is the same for both. Host-only
/// is the default because it is the honest default: it is the check that ran, and the report says what
/// did not.
struct ComponentSheet: View {
    /// What the user is adding, mirroring `ComponentKind` in the core.
    ///
    /// This is the question the component model turns on, and the reason it is asked rather than
    /// inferred: the two kinds share no skeleton. A driver gets a bus seam and a fake bus, so a host
    /// test can stand in for the device that is not there; a library gets plain code and an ordinary
    /// unit test, because there is nothing to stand in for.
    enum Kind: String, CaseIterable, Identifiable {
        case driver
        case library

        var id: String { rawValue }

        /// The kind in the user's terms rather than the tool's.
        var label: String {
            switch self {
            case .driver: return "Device driver"
            case .library: return "Library"
            }
        }

        /// The one sentence that tells the two apart.
        var hint: String {
            switch self {
            case .driver:
                return "One device on one bus: a protocol, written from its datasheet."
            case .library:
                return "Pure code — an algorithm, a filter, a codec — with no device and no bus."
            }
        }

        /// What the name field is asking for.
        var nameLabel: String { self == .driver ? "Device" : "Component" }

        /// What the empty name field suggests.
        var nameHint: String { self == .driver ? "sps30" : "moving_average" }
    }

    enum Mode {
        /// A new component: a kind, a name, and — for a driver — a bus.
        case add
        /// An existing component: what the user knows about it, which is how its code is changed.
        case edit(SubprojectInfo)

        var isAdd: Bool {
            if case .add = self { return true }
            return false
        }
    }

    enum Result {
        case ok(String)
        case failed(String)
    }

    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    let mode: Mode
    let root: String
    let done: (Result) -> Void

    /// The kind an **existing** component states about itself — read by the analyzer from the
    /// component's own `CMakeLists.txt`.
    ///
    /// `nil` for a component that states none, which is what one written by hand looks like. The sheet
    /// then says nothing about a bus rather than assuming one, and the prompt falls back to what the
    /// component's own files show.
    private let statedKind: Kind?

    @State private var kind: Kind = .driver
    @State private var name: String
    @State private var bus = "i2c"
    @State private var chip = ""
    @State private var chips: [Platform] = []
    @State private var described: String = ""
    @State private var running = false
    @State private var step = ""
    @State private var failure: String?

    init(mode: Mode, root: String, done: @escaping (Result) -> Void) {
        self.mode = mode
        self.root = root
        self.done = done
        switch mode {
        case .add:
            _name = State(initialValue: "")
            statedKind = nil
        case .edit(let component):
            _name = State(initialValue: component.name)
            statedKind = component.componentKind.flatMap(Kind.init(rawValue:))
        }
    }

    /// The kind this sheet is speaking about: the picker's choice for a new component, the component's
    /// own statement for an existing one.
    private var effectiveKind: Kind? { mode.isAdd ? kind : statedKind }

    private var subtitle: String {
        if mode.isAdd {
            return "The skeleton follows from the kind; the code is written from what you say here, "
                 + "and then its host test has to pass."
        }
        switch statedKind {
        case .driver:
            return "A device driver: its protocol is written from what you say here, then its host "
                 + "test has to pass."
        case .library:
            return "A library component: its code is written from what you say here, then its host "
                 + "test has to pass."
        case nil:
            return "Its code is written from what you say here, then its host test has to pass."
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(mode.isAdd ? "Add a component" : name)
                    .font(.title3.weight(.semibold))
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
            }

            if mode.isAdd {
                VStack(alignment: .leading, spacing: 6) {
                    SectionHeading("What it is")
                    Picker("", selection: $kind) {
                        ForEach(Kind.allCases) { Text($0.label).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .disabled(running)
                    Text(kind.hint)
                        .font(.caption2)
                        .foregroundStyle(theme.textTertiary)
                }
                VStack(alignment: .leading, spacing: 6) {
                    SectionHeading(kind.nameLabel)
                    TextField(kind.nameHint, text: $name)
                        .textFieldStyle(.roundedBorder)
                        .disabled(running)
                }
                // The bus is a **driver's** fact, so only a driver is asked for one. A library is code:
                // a bus field on it would be a bus it does not have.
                if kind == .driver {
                    VStack(alignment: .leading, spacing: 6) {
                        SectionHeading("Bus")
                        Picker("", selection: $bus) {
                            Text("I²C").tag("i2c")
                            Text("SPI").tag("spi")
                            Text("UART").tag("uart")
                        }
                        .pickerStyle(.segmented)
                        .labelsHidden()
                        .disabled(running)
                    }
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                SectionHeading("Verify against")
                Picker("", selection: $chip) {
                    Text("Host only").tag("")
                    ForEach(chips) { Text($0.name).tag($0.id) }
                }
                .labelsHidden()
                .disabled(running)
                .help("The host test always runs. Naming a chip adds `idf.py build` for it as a second "
                      + "gate — slower, and it catches what a host test cannot.")
                Text(chip.isEmpty
                     ? "The host test is the whole check: seconds, no board, no IDF — and it does not "
                       + "compile the component against the real driver."
                     : "Adds `idf.py build` for \(chip) as a second gate. It catches what a host test "
                       + "cannot — a wrong REQUIRES, a link error against the IDF driver.")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            }


            VStack(alignment: .leading, spacing: 6) {
                SectionHeading(boxHeading)
                TextEditor(text: $described)
                    .font(.system(.body, design: .monospaced))
                    .frame(height: 140)
                    .overlay(
                        RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1)
                    )
                    .disabled(running)
                Text(descriptionHint)
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            }

            if let failure {
                Text(failure)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .lineLimit(8)
            }
            if running {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(step).font(.caption).foregroundStyle(theme.textSecondary)
                }
            }

            HStack(spacing: 8) {
                Spacer(minLength: 0)
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(running)
                if mode.isAdd {
                    Button("Stub only") { addStubOnly() }
                        .disabled(!canRun)
                }
                Button(actionLabel) { write() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canRun)
            }
        }
        .padding(20)
        .frame(width: 520)
        .background(theme.background)
        .task { await loadChips() }
    }

    /// What the button does, said rather than left to "Write" — which would read as writing code that
    /// this sheet does not write.
    private var actionLabel: String {
        if mode.isAdd { return "Add & Write" }
        return "Write"
    }

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canRun: Bool {
        !running && !trimmedName.isEmpty && !trimmedName.contains("/")
    }

    /// What the box is for, in this component's terms — and what an empty one means.
    ///
    /// What the box is asking for, which is not the same question for every component.
    private var boxHeading: String {
        return effectiveKind == .driver ? "What the device is" : "What this should do"
    }

    /// A driver's answer comes off a datasheet and a library's comes from the caller's head, so the same
    /// box would otherwise invite the wrong one: "what does register 0x1F return" has no answer for a
    /// filter.
    private var descriptionHint: String {
        switch (effectiveKind, described.isEmpty) {
        case (.driver, true):
            return "Command words, register addresses, reply lengths, checksums, byte order — the things "
                 + "that come off a datasheet. Left empty, nothing is invented: the stub keeps its TODOs."
        case (.driver, false):
            return "Written from this alone: the protocol is not looked up, and nothing you did not say "
                 + "is added."
        case (_, true):
            return "What it computes, and with what inputs and outputs, and the cases where it must "
                 + "refuse — the part no code can supply. Left empty, nothing is invented: the stub "
                 + "keeps its TODOs."
        case (_, false):
            return "Written from this alone: no algorithm, unit or constant you did not give is assumed."
        }
    }

    // MARK: - Acting

    /// The chips the registry knows — the same source the build leg reads its targets from.
    ///
    /// An empty list (a machine whose registry has never been loaded) leaves the picker on "Host only",
    /// which is still a truthful answer: the host test runs whatever is named here.
    private func loadChips() async {
        guard chips.isEmpty else { return }
        chips = await bridge.fetchPlatforms()
            .filter { $0.os == "esp-idf" && $0.kind == "chip" }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// The bus to pass: a driver's, and nothing for a library. A bus handed to a library is a contract
    /// that lies, so it is not handed one.
    private var busArgument: String { effectiveKind == .driver ? bus : "" }


    /// Add the stub and stop: the skeleton, with no model call. For when the device's facts (or the
    /// algorithm) are not in hand yet and the component should still exist.
    private func addStubOnly() {
        running = true
        failure = nil
        step = "Adding the stub…"
        Task {
            let (result, error) = await bridge.idfAddComponent(
                root: root, name: trimmedName, kind: kind.rawValue, bus: busArgument
            )
            await MainActor.run {
                running = false
                if let error {
                    failure = error
                    return
                }
                let files = (result?["files"] as? [String]) ?? []
                let kindWord = kind == .driver ? "protocol" : "code"
                dismiss()
                done(.ok("Added components/\(trimmedName) — \(files.count) files, its \(kindWord) "
                         + "still a TODO. Write it when you have what it needs."))
            }
        }
    }

    /// Add (when this is a new component) and then write the code. The write is the one step that needs
    /// a model, and the one that has to pass the component's host test to be kept.
    private func write() {
        running = true
        failure = nil
        Task {
            if mode.isAdd {
                await MainActor.run { step = "Adding the stub…" }
                let (_, error) = await bridge.idfAddComponent(
                    root: root, name: trimmedName, kind: kind.rawValue, bus: busArgument
                )
                if let error {
                    await MainActor.run {
                        running = false
                        failure = error
                    }
                    return
                }
            }
            let what = kind == .driver ? "Writing the protocol" : "Writing the code"
            await MainActor.run {
                step = chip.isEmpty
                    ? "\(what), then its host test…"
                    : "\(what), then its host test and the \(chip) build…"
            }
            let (result, error) = await bridge.idfComponentEdit(
                root: root,
                name: trimmedName,
                instruction: described,
                platform: chip.isEmpty ? nil : chip
            )
            await MainActor.run {
                running = false
                step = ""
                if let error {
                    failure = error
                    return
                }
                let kept = (result?["success"] as? Bool) ?? false
                let summary = (result?["output"] as? String) ?? ""
                let chipBuild = (result?["chip_build"] as? String) ?? ""
                if kept {
                    dismiss()
                    done(.ok("Wrote \(trimmedName).\n\(summary)\n\(chipBuild)"))
                } else {
                    // The gate refused the change and restored it. That is a *result*, not a failure of
                    // the tool — so it is reported as what happened, with what was checked.
                    failure = "The change was not kept.\n\(summary)\n\(chipBuild)"
                }
            }
        }
    }
}

