import Foundation

/// Domain service for LLM configuration. Repository pattern: owns all LLM
/// config FFI calls (`config/getAll`, `config/set`) so the view layer stays
/// presentation-only.
actor LLMService {
    let backend: any UIBackend

    init(backend: any UIBackend) {
        self.backend = backend
    }

    /// Load LLM settings from the Rust core via `config/getAll`.
    /// Returns a fresh LLMConfig populated from the backend, or the defaults
    /// if the fetch fails.
    func fetchConfig() async -> LLMConfig {
        do {
            let command = try MessageSerializer.encode(command: "config/getAll")
            let reply = try await backend.send(command)
            let dict = (try JSONSerialization.jsonObject(with: reply) as? [String: Any])?["config"] as? [String: Any] ?? [:]
            var config = LLMConfig()
            config.apiKey = dict["deepseek.api_key"] as? String ?? ""
            config.planningModel = dict["deepseek.planning_model"] as? String ?? "deepseek-v4-pro"
            config.codingModel = dict["deepseek.coding_model"] as? String ?? "deepseek-v4-pro"
            return config
        } catch {
            return LLMConfig()
        }
    }

    /// Save one config key via `config/set`.
    func saveKey(_ key: String, _ value: String) async -> Bool {
        do {
            let body: [String: Any] = [
                "method": "config/set",
                "params": ["key": key, "value": value]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return (try JSONSerialization.jsonObject(with: reply) as? [String: Any])?["success"] as? Bool ?? false
        } catch {
            return false
        }
    }

    /// Save all three LLM settings.
    func save(_ config: LLMConfig) async -> Bool {
        let keyOk = await saveKey("deepseek.api_key", config.apiKey)
        let planningOk = await saveKey("deepseek.planning_model", config.planningModel)
        let codingOk = await saveKey("deepseek.coding_model", config.codingModel)
        return keyOk && planningOk && codingOk
    }
}