import SwiftUI

/// Authoring a **contract**: write the Rust, see what it declares, then write it into the project.
///
/// The two steps are separate on purpose, and they are the tool's own shape: `validate` is read-only
/// and answers "would the drift measure see this?" — the rules are what the measure needs (it parses,
/// it declares at least one implementable trait, it implements nothing, no trait name twice) — and
/// `write` is the one that touches disk, adding the file **and** the `pub mod`/`pub use` lines in
/// `hal/mod.rs` that make it visible. A file nothing declares is a contract that behaves as if it
/// were absent, which is the failure this window exists to prevent.
struct EmbeddedHalContractSheet: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss
    let projectRoot: String

    /// Called after a successful write, so the caller can re-read coverage (a new contract is new
    /// work for every backend).
    var onWritten: () async -> Void = {}

    @State private var filename = "sensor.rs"
    @State private var source = EmbeddedHalContractSheet.starter
    @State private var traits: [String] = []
    @State private var status: String?
    @State private var errorText: String?
    @State private var busy = false

    /// The empty-state contract: a commented shape to fill in, not a blank page. Deleting the
    /// comments is the first edit, and the doc comment on the trait is what the project's own
    /// convention asks for.
    static let starter = """
    //! A sensor on this family's bus.
    //!
    //! The bus itself is configured by the family's backend; this trait only reads it.

    pub trait Sensor {
        /// The last reading, in tenths of a degree Celsius (negative below zero).
        fn read_deci_celsius(&mut self) -> i32;
    }
    """

    /// The traits a validation summary declares, as display lines: `Sensor — read_deci_celsius`.
    ///
    /// Pure and static so it can be tested against the tool's exact payload without a core.
    static func describeTraits(summary: [String: Any]) -> [String] {
        let traits = (summary["traits"] as? [[String: Any]]) ?? []
        return traits.map { entry in
            let name = (entry["trait"] as? String) ?? "?"
            let methods = (entry["methods"] as? [String])?.joined(separator: ", ") ?? ""
            return methods.isEmpty ? name : "\(name) — \(methods)"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("New contract").font(.headline)
            Text("A contract is a trait every backend implements. It is written into the contract crate and declared there, so the drift measure sees it.")
                .font(.caption).foregroundStyle(.secondary)

            HStack(spacing: 8) {
                Text("File").font(.caption)
                TextField("sensor.rs", text: $filename)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 180)
                Text("→ crates/*-hal/src/hal/")
                    .font(.caption2).foregroundStyle(.secondary)
            }

            TextEditor(text: $source)
                .font(.system(.caption, design: .monospaced))
                .frame(minHeight: 220)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 0.5))

            if !traits.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Declares").font(.caption.weight(.semibold))
                    ForEach(traits, id: \.self) { line in
                        Text(line).font(.caption2.monospaced()).foregroundStyle(.secondary)
                    }
                }
            }
            if let errorText {
                Text(errorText)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let status {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }

            HStack(spacing: 8) {
                Button("Validate") { Task { await validate() } }
                    .disabled(busy || source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Spacer()
                Button("Cancel") { dismiss() }
                Button {
                    Task { await write() }
                } label: {
                    Label("Write contract", systemImage: "square.and.arrow.down")
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy || traits.isEmpty)
            }
        }
        .padding(14)
        .frame(minWidth: 560)
    }

    @MainActor
    private func validate() async {
        busy = true
        errorText = nil
        status = nil
        defer { busy = false }
        let (summary, error) = await bridge.embeddedHalValidateContract(content: source)
        if let error {
            traits = []
            errorText = error
            return
        }
        traits = summary.map(Self.describeTraits) ?? []
        status = "Valid — this is what the measure will see."
    }

    @MainActor
    private func write() async {
        busy = true
        errorText = nil
        defer { busy = false }
        let (result, error) = await bridge.embeddedHalWriteContract(
            root: projectRoot,
            filename: filename,
            content: source
        )
        if let error {
            errorText = error
            return
        }
        let wired = (result?["wired"] as? Bool) ?? false
        let unchanged = (result?["unchanged"] as? Bool) ?? false
        status = unchanged
            ? "Already there, byte for byte — nothing to write."
            : "Written. \(wired ? "Declared and re-exported." : "Already declared.")"
        await onWritten()
    }
}
