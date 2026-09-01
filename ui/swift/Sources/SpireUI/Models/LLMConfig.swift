import Foundation

/// Mirrors the Rust LLM settings persisted in the memory graph.
struct LLMConfig: Codable {
    var apiKey: String = ""
    var planningModel: String = "deepseek-chat"
    var codingModel: String = "deepseek-chat"
    /// Tavily web-search API key (used by the `search/web` tools).
    var tavilyApiKey: String = ""

    /// The model choices available in the settings UI.
    ///
    /// `deepseek-chat` is the DEFAULT for planning/coding: verified live
    /// (2026-08-17) that the reasoning models (`deepseek-v4-pro`/`-flash`/`r1`)
    /// return EMPTY `content` + `finish_reason="length"` (all tokens in
    /// `reasoning_content`) for Spire's structured-JSON planning prompts,
    /// which the parser cannot use. `deepseek-chat` returns the JSON in
    /// `content`.
    static let modelChoices = [
        "deepseek-chat",
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "deepseek-r1",
    ]

    enum CodingKeys: String, CodingKey {
        case apiKey = "deepseek.api_key"
        case planningModel = "deepseek.planning_model"
        case codingModel = "deepseek.coding_model"
        case tavilyApiKey = "tavily.api_key"
    }
}
