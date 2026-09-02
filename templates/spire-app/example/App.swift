import SwiftUI

/// Reference SwiftUI shell — mirror the scaffold's ContentView then grow it:
/// chat/RAG panels call `core.send(...)`; build/status flows can be added as
/// separate services once the core exposes the methods.
struct ExampleView: View {
    @Environment(CoreBridge.self) private var core
    @State private var reply = "no reply yet"

    var body: some View {
        VStack(spacing: 16) {
            Text("Spire Example")
                .font(.largeTitle.weight(.semibold))
            Text(core.statusText)
                .foregroundStyle(.secondary)
            Button("Say hello") {
                reply = core.send("\"world\"") ?? "no core"
            }
            Text(reply)
                .font(.body.monospaced())
                .textSelection(.enabled)
        }
        .padding(32)
    }
}
