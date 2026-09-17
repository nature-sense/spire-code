import SwiftUI

/// The **Rust** backends, on the verification surface: one row per board family with what each
/// interface still owes, and the fill action that closes it.
///
/// The rows come from `hal_missing_impls` — the same coverage the maturity chips, the HAL build
/// gate and the fill plan read — so a backend shown as `stub` here is exactly what keeps that
/// platform's build disabled, and filling it flips both.
///
/// The fill is the Rust cascade rather than the C++ pair-file flow: a Rust backend is one file per
/// family that implements *every* trait, so the plan is one item per file with one prompt, and the
/// apply is one model call whose answer is gated (every pending trait implemented by name, no
/// `unimplemented!()` left) before it is written. Nothing is written that the gate refuses.
struct EmbeddedHalBackendsSection: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let projectRoot: String

    /// The board families the registry knows (`esp32`, `rp2040`).
    ///
    /// A family, not a platform id: the coverage map is keyed by the backend crate's suffix, and one
    /// backend serves every variant of its family.
    @State private var families: Set<String> = []
    /// Every embedded platform the registry has, for the add-board menu. Kept whole (id *and* name)
    /// because the id is what the tool takes and the name is what the user reads.
    @State private var boards: [Platform] = []
    @State private var plan: [[String: Any]] = []
    @State private var status: String?
    @State private var busy = false
    @State private var showingContract = false

    /// The boards this project could still add: embedded, with a family no backend crate covers yet,
    /// one entry per family (two variants of a board are one backend, so offering both would offer
    /// the same crate twice).
    ///
    /// Static and pure so it can be tested without a running core — the same reason the wizard's
    /// platform filter is not re-implemented in the view.
    static func addableBoards(platforms: [Platform], presentFamilies: Set<String>) -> [Platform] {
        var seen: Set<String> = presentFamilies
        var out: [Platform] = []
        for platform in platforms where platform.embedded {
            guard let family = platform.family else { continue }
            if seen.contains(family) { continue }
            seen.insert(family)
            out.append(platform)
        }
        return out.sorted { $0.id < $1.id }
    }

    private var addable: [Platform] {
        Self.addableBoards(platforms: boards, presentFamilies: Set(bridge.halFunctionGaps.keys))
    }

    private var rows: [(family: String, interfaces: [(name: String, gaps: SpireBridge.HalInterfaceGaps)])] {
        bridge.halFunctionGaps
            .filter { families.contains($0.key) && !$0.value.isEmpty }
            .map { family, interfaces in
                (family, interfaces.map { ($0.key, $0.value) }.sorted { $0.0 < $1.0 })
            }
            .sorted { $0.family < $1.family }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text("Rust backends").font(.headline)
                Spacer()
                Button {
                    Task { await planFill() }
                } label: {
                    if busy {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text("Working…")
                        }
                    } else {
                        Label(plan.isEmpty ? "Plan fill" : "Re-plan", systemImage: "wand.and.stars")
                    }
                }
                .buttonStyle(.bordered)
                .disabled(busy || rows.isEmpty)

                // A contract is work for *every* backend, so authoring one belongs where the
                // backends are listed: after a write, the rows above gain an interface.
                Button {
                    showingContract = true
                } label: {
                    Label("New contract", systemImage: "doc.badge.plus")
                }
                .buttonStyle(.bordered)
                .disabled(busy)

                // Adding a board is the other half of authoring: it emits the backend crate and its
                // workspace member, after which the rows above gain a family and the plan gains a
                // file. Offered only for families this project does not have yet — the tool refuses
                // a duplicate, and a menu that offers what it will refuse is a lie.
                if !addable.isEmpty {
                    Menu {
                        ForEach(addable, id: \.id) { board in
                            Button {
                                Task { await addBoard(board) }
                            } label: {
                                Text(board.name.isEmpty ? board.id : "\(board.name) (\(board.id))")
                            }
                        }
                    } label: {
                        Label("Add board", systemImage: "plus.rectangle.on.folder")
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .disabled(busy)
                }
            }

            if rows.isEmpty {
                Text("No Rust backend for this project — this is the C++ HAL surface.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(rows, id: \.family) { row in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(row.family).font(.callout.weight(.semibold))
                        ForEach(row.interfaces, id: \.name) { iface in
                            HStack(spacing: 6) {
                                maturityBadge(iface.gaps.maturity)
                                Text(iface.name).font(.caption.monospaced())
                                if iface.gaps.missing.isEmpty {
                                    Text(iface.gaps.maturity == "stub"
                                         ? "declared, nothing written yet"
                                         : "complete")
                                        .font(.caption2).foregroundStyle(.secondary)
                                } else {
                                    Text("missing: \(iface.gaps.missing.joined(separator: ", "))")
                                        .font(.caption2).foregroundStyle(.secondary)
                                }
                                Spacer(minLength: 0)
                            }
                        }
                    }
                    .padding(8)
                    .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
                }
            }

            planSection

            if let status {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }
        }
        .task {
            let platforms = await bridge.fetchPlatforms()
            boards = platforms
            families = Set(platforms.filter(\.embedded).compactMap(\.family))
        }
        .sheet(isPresented: $showingContract) {
            EmbeddedHalContractSheet(projectRoot: projectRoot) {
                // A new contract is new work for every backend, and every backend's build gate
                // reads the same measure — so the rows and the platform status both catch up here.
                await bridge.refreshHalData(root: projectRoot)
            }
        }
    }

    /// The reviewed plan: what will be written, one line per pending trait, before the model is
    /// asked for anything.
    @ViewBuilder
    private var planSection: some View {
        if !plan.isEmpty {
            Divider()
            Text("\(plan.count) file\(plan.count == 1 ? "" : "s") to write")
                .font(.caption.weight(.semibold))
            ForEach(Array(plan.enumerated()), id: \.offset) { _, item in
                VStack(alignment: .leading, spacing: 2) {
                    Text(fileName(item)).font(.caption.monospaced())
                    ForEach(pending(item), id: \.self) { line in
                        Text(line).font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
            HStack(spacing: 8) {
                Text("Each file goes to the model once; the answer is gated, then the project is re-measured.")
                    .font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Cancel") { plan = []; status = nil }
                Button {
                    Task { await applyFill() }
                } label: {
                    Label("Fill", systemImage: "sparkles")
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
            }
        }
    }

    private func fileName(_ item: [String: Any]) -> String {
        let path = (item["file"] as? String) ?? (item["crate"] as? String) ?? "?"
        return (path as NSString).lastPathComponent
    }

    /// `led [stub]: Led — set`, one line per pending trait: what will be asked for, in the user's
    /// terms rather than the model's.
    private func pending(_ item: [String: Any]) -> [String] {
        guard let entries = item["pending"] as? [[String: Any]] else { return [] }
        return entries.map { entry in
            let iface = (entry["interface"] as? String) ?? "?"
            let state = (entry["status"] as? String) ?? "?"
            let traitName = (entry["trait"] as? String) ?? "?"
            let methods = (entry["methods"] as? [String])?.joined(separator: ", ") ?? ""
            return "\(iface) [\(state)]: \(traitName) — \(methods)"
        }
    }

    private func maturityBadge(_ maturity: String) -> some View {
        let color: Color = switch maturity {
        case "implemented": .green
        case "stub": .blue
        case "partial": .orange
        default: .red
        }
        return Text(maturity.uppercased())
            .font(.caption2.weight(.bold))
            .foregroundStyle(color)
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(color.opacity(0.12), in: Capsule())
    }

    /// Add a board: emit its backend crate and workspace member, then re-read the coverage so the
    /// rows above gain the family — and the board disappears from this menu, because its family is now
    /// present. Nothing is claimed on the tool's behalf: `written`/`workspace_member` are reported as
    /// they came back, and a refusal (a duplicate, a non-board platform) is shown as the reason.
    @MainActor
    private func addBoard(_ board: Platform) async {
        busy = true
        status = nil
        defer { busy = false }
        let (result, error) = await bridge.embeddedHalAddPlatform(root: projectRoot, platform: board.id)
        if let error {
            status = "Could not add \(board.id): \(error)"
            return
        }
        guard let result else {
            status = "Could not add \(board.id): no answer from the core."
            return
        }
        var lines: [String] = []
        if let crate = result["crate"] as? String {
            lines.append("added \(crate) for family \(result["family"] as? String ?? "?")")
        }
        if let member = result["workspace_member"] as? String {
            lines.append("workspace member: \(member)")
        }
        if let note = result["note"] as? String {
            lines.append(note)
        }
        status = lines.isEmpty ? "Added \(board.id)." : lines.joined(separator: "\n")
        plan = []
        await bridge.refreshHalData(root: projectRoot)
    }

    @MainActor
    private func planFill() async {
        busy = true
        status = nil
        defer { busy = false }
        let (items, error) = await bridge.embeddedHalFillPlan(root: projectRoot)
        if let error {
            status = error
            plan = []
            return
        }
        plan = items
        if items.isEmpty {
            status = "Nothing to fill — every backend already measures implemented."
        }
    }

    @MainActor
    private func applyFill() async {
        busy = true
        defer { busy = false }
        let (applied, failures, error) = await bridge.embeddedHalFillApply(root: projectRoot, plan: plan)
        if let error {
            status = error
            return
        }
        var lines: [String] = []
        let done = applied.compactMap { ($0["family"] as? String) ?? ($0["file"] as? String) }
        if !done.isEmpty { lines.append("filled: \(done.joined(separator: ", "))") }
        for failure in failures {
            let file = (failure["file"] as? String).map { ($0 as NSString).lastPathComponent } ?? "?"
            let reason = (failure["reason"] as? String) ?? "refused"
            lines.append("\(file): \(reason)")
        }
        status = lines.isEmpty ? "Nothing written." : lines.joined(separator: "\n")
        plan = []
        // Re-read the coverage so the rows above (and the platform's build gate) agree with what is
        // on disk now. The apply re-measured already; this is the UI catching up to it.
        await bridge.refreshHalData(root: projectRoot)
    }
}
