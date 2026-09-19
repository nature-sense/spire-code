import SwiftUI

/// Growing a **container**: the two operations that are *routine* once the framework exists, because
/// everything after the framework is one board or one device at a time.
///
/// Neither choice is the wizard's to make: a BSP arrives when a board has no upstream one, and a
/// driver when no upstream crate speaks `embedded-hal` 1.x — facts the user has and the scaffold
/// cannot infer. What the scaffold *can* do is emit both as typed `todo!()`s, so the container and
/// every application built against it still compile while the pins and protocols are unknown.
struct EmbeddedContainerSheet: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    /// The container's directory — the project root the pane is showing.
    let projectRoot: String

    @State private var platforms: [Platform] = []
    @State private var board = ""
    @State private var device = ""
    @State private var bus = "i2c"

    @State private var busy = false
    @State private var outcome: String?
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Container").font(.headline)
                Spacer()
                Button("Done") { dismiss() }
            }
            Text("The actor framework, the peripheral drivers, and a BSP crate per board — one library that applications are built *against* rather than contain.")
                .font(.caption)
                .foregroundStyle(.secondary)

            Divider()

            section(
                title: "Add a board (BSP)",
                detail: "For a board with no upstream BSP. Its facts — which pin the LED is on, whether it is active-low — are emitted as typed `todo!()`s, so nothing stops building while they are unknown."
            ) {
                HStack(spacing: 8) {
                    Picker("", selection: $board) {
                        Text("Choose a board…").tag("")
                        ForEach(boardChoices, id: \.id) { platform in
                            Text(platform.name).tag(platform.id)
                        }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 260)
                    Button("Add BSP") { Task { await addBsp() } }
                        .disabled(busy || board.isEmpty)
                }
            }

            Divider()

            section(
                title: "Add a device driver",
                detail: "For a device with no upstream crate that speaks `embedded-hal` 1.x. The module is generic over the bus; the protocol is what the fill writes, and the emitted host test is where its bytes get asserted."
            ) {
                HStack(spacing: 8) {
                    TextField("device (e.g. bme280)", text: $device)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 200)
                    Picker("", selection: $bus) {
                        Text("I²C").tag("i2c")
                        Text("SPI").tag("spi")
                    }
                    .labelsHidden()
                    .frame(maxWidth: 100)
                    Button("Add driver") { Task { await addDriver() } }
                        .disabled(busy || device.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }

            if busy {
                ProgressView().controlSize(.small)
            }
            if let error {
                Text(error).font(.caption).foregroundStyle(.red)
            } else if let outcome {
                Text(outcome).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(16)
        .frame(width: 580, alignment: .leading)
        .task { platforms = await bridge.fetchPlatforms() }
    }

    /// One of the two operations, with its own explanation.
    @ViewBuilder
    private func section<Content: View>(
        title: String,
        detail: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.subheadline.weight(.semibold))
            Text(detail).font(.caption).foregroundStyle(.secondary)
            content()
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
    }

    /// The boards the registry knows: embedded platforms only, which is the set a BSP can exist for.
    /// A host or a Linux cross-target is `embedded == false` and is filtered out by the registry's own
    /// rule rather than a second copy of it here.
    private var boardChoices: [Platform] {
        platforms.filter(\.embedded)
    }

    private func addBsp() async {
        busy = true
        error = nil
        outcome = nil
        let (json, err) = await bridge.embeddedAddBsp(root: projectRoot, board: board)
        busy = false
        if let err {
            error = err
            return
        }
        let crate = (json?["crate"] as? String) ?? "the BSP"
        let written = (json?["written"] as? [String]) ?? []
        outcome = "Added \(crate) — \(written.joined(separator: ", "))"
    }

    private func addDriver() async {
        busy = true
        error = nil
        outcome = nil
        let name = device.trimmingCharacters(in: .whitespaces)
        let (json, err) = await bridge.embeddedAddDriver(root: projectRoot, device: name, bus: bus)
        busy = false
        if let err {
            error = err
            return
        }
        let written = (json?["written"] as? [String]) ?? []
        outcome = "Added \(name) — \(written.joined(separator: ", "))"
    }
}
