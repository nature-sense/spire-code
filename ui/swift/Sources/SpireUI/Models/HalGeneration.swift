import Foundation

/// Result of `hal_build_impl_prompt`: the SEMANTIC module-pair implementation
/// prompt plus the deterministic clean declaration header it targets. Read-only
/// — lets the user preview exactly what the LLM will be asked before any
/// generation happens.
struct HalImplPromptResult: Codable {
    let interface: String
    let platform: String
    let className: String
    let header: String?
    let prompt: String
    let summary: String

    enum CodingKeys: String, CodingKey {
        case interface, platform, prompt, summary, header
        case className = "class_name"
    }
}

/// Result of `hal_generate_impl`: the written module-pair files (deterministic
/// clean header + LLM-written `.cpp`), the meson build-gate command + status,
/// any stale stubs that were removed, and the C++ syntax-check verdict.
struct HalGenerateImplResult: Codable {
    let interface: String
    let platform: String
    let className: String
    let header: String?
    let written: [String]
    let removedStubs: [String]
    let syntax: String
    let gate: String
    let gateStatus: String

    enum CodingKeys: String, CodingKey {
        case interface, platform, written, gate, syntax
        case className = "class_name"
        case header
        case removedStubs = "removed_stubs"
        case gateStatus = "gate_status"
    }
}

/// Proposed module pair from `hal_generate_impl_plan`: the deterministic clean
/// declaration header + the LLM-written `.cpp`, previewed for approval BEFORE
/// any write. `id` lets it drive a sheet item.
struct HalGenerateImplPlan: Codable, Identifiable {
    var id: String { interface + "/" + platform }
    let interface: String
    let platform: String
    let className: String
    let hppPath: String
    let cppPath: String
    let header: String
    let source: String
    let prompt: String
    let syntax: String

    enum CodingKeys: String, CodingKey {
        case interface, platform, header, source, prompt, syntax
        case className = "class_name"
        case hppPath = "hpp_path"
        case cppPath = "cpp_path"
    }
}

/// Result of `hal_generate_impl_apply`: the written module-pair paths, removed
/// stubs and the meson compile-gate verdict.
struct HalGenerateImplApplyResult: Codable {
    let interface: String
    let platform: String
    let className: String
    let written: [String]
    let removedStubs: [String]
    let gate: String
    let gateStatus: String

    enum CodingKeys: String, CodingKey {
        case interface, platform, written, gate
        case className = "class_name"
        case removedStubs = "removed_stubs"
        case gateStatus = "gate_status"
    }
}
