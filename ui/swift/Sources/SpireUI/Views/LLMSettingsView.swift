import SwiftUI

/// Modal sheet for configuring LLM settings (API key + planning/coding models).
struct LLMSettingsView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(\.dismiss) private var dismiss

    @State private var apiKey = ""
    @State private var planningModel = "deepseek-v4-pro"
    @State private var codingModel = "deepseek-v4-pro"
    @State private var tavilyApiKey = ""
    @State private var saving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            // Header
            HStack {
                Image(systemName: "key.horizontal")
                Text("LLM Settings").font(.title2.bold())
            }

            // API key
            VStack(alignment: .leading) {
                Text("DeepSeek API Key").font(.headline)
                SecureField("sk-...", text: $apiKey)
                    .textFieldStyle(.roundedBorder)
            }

            // Tavily web-search API key (for the search/web tools)
            VStack(alignment: .leading) {
                Text("Tavily API Key (web search)").font(.headline)
                SecureField("tvly-...", text: $tavilyApiKey)
                    .textFieldStyle(.roundedBorder)
            }

            // Planning model
            VStack(alignment: .leading) {
                Text("Planning Model").font(.headline)
                Picker("Planning", selection: $planningModel) {
                    ForEach(LLMConfig.modelChoices, id: \.self) { model in
                        Text(model).tag(model)
                    }
                }
                .pickerStyle(.menu)
            }

            // Coding model
            VStack(alignment: .leading) {
                Text("Coding Model").font(.headline)
                Picker("Coding", selection: $codingModel) {
                    ForEach(LLMConfig.modelChoices, id: \.self) { model in
                        Text(model).tag(model)
                    }
                }
                .pickerStyle(.menu)
            }

            Spacer()

            // Buttons
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button(saving ? "Saving…" : "Save") {
                    save()
                }
                .buttonStyle(.borderedProminent)
                .disabled(saving || apiKey.isEmpty)
            }
        }
        .padding(24)
        .frame(width: 480, height: 400)
        .onAppear { load() }
    }

    private func load() {
        apiKey = bridge.llmConfig.apiKey
        planningModel = bridge.llmConfig.planningModel
        codingModel = bridge.llmConfig.codingModel
        tavilyApiKey = bridge.llmConfig.tavilyApiKey
    }

    private func save() {
        saving = true
        Task {
            let ok = await bridge.saveLlmConfig(LLMConfig(
                apiKey: apiKey,
                planningModel: planningModel,
                codingModel: codingModel,
                tavilyApiKey: tavilyApiKey
            ))
            saving = false
            if ok { dismiss() }
        }
    }
}