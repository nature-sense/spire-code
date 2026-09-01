/// Mirrors the Rust `hal_verify` payload: one issue (rich: message + suggested fix).
struct HalVerificationIssue: Codable, Identifiable {
    var id: String { title + path + message }
    var severity: String  // "error" | "warning" | "info"
    var title: String
    var path: String
    var message: String
    var suggestedFix: String

    enum CodingKeys: String, CodingKey {
        case severity, title, path, message
        case suggestedFix = "suggested_fix"
    }
}

import Foundation

/// Mirrors the Rust `hal_report` payload: contracts + core datatypes.
struct HalDocReport: Codable {
    var contracts: [HalContractDoc]
    var types: [HalTypeDoc]
}

/// One contract page (docs + methods + per-platform status).
struct HalContractDoc: Codable, Identifiable {
    var id: String { stem }
    var stem: String
    var className: String
    var contractId: String
    var brief: String
    var tags: [HalDocTag]
    var prose: String
    var header: String
    var methods: [HalMethodDoc]
    var usesTypes: [String]
    var platforms: [HalPlatformDoc]

    enum CodingKeys: String, CodingKey {
        case stem
        case className = "class_name"
        case contractId = "id"
        case brief, tags, prose, header, methods
        case usesTypes = "uses_types"
        case platforms
    }
}

/// A contract method with its attached structured tags + prose.
struct HalMethodDoc: Codable, Identifiable {
    var id: String { name }
    var name: String
    var returnType: String
    var params: String
    var tags: [HalDocTag]
    var prose: String

    enum CodingKeys: String, CodingKey {
        case name
        case returnType = "return_type"
        case params, tags, prose
    }
}

/// A parsed structured-doc tag (`@brief` → name "@brief", key "brief", value …).
struct HalDocTag: Codable {
    var name: String
    var key: String
    var value: String
}

/// Per-platform implementation status for one contract.
struct HalPlatformDoc: Codable, Identifiable {
    var id: String { platform }
    var platform: String
    var implemented: Bool
    var hasImpl: Bool
    var missing: [String]
    var drifted: [String]

    enum CodingKeys: String, CodingKey {
        case platform, implemented
        case hasImpl = "has_impl"
        case missing, drifted
    }
}

/// One documented struct field (member-level annotated docs).
struct HalFieldDoc: Codable, Identifiable {
    var id: String { name }
    var name: String
    var typeName: String
    var tags: [HalDocTag]
    var prose: String

    enum CodingKeys: String, CodingKey {
        case name
        case typeName = "type_name"
        case tags, prose
    }
}

/// One core datatype page.
struct HalTypeDoc: Codable, Identifiable {
    var id: String { name }
    var name: String
    var header: String
    var brief: String
    var tags: [HalDocTag]
    var prose: String
    var fields: [HalFieldDoc]
}


/// Mirrors the Rust `hal_doc_lint` payload: one documentation-lint issue.
struct HalDocLintIssue: Codable, Identifiable {
    var id: String { symbol + title + path }
    var severity: String
    var title: String
    var path: String
    var symbol: String
    var message: String
    var fixPrompt: String

    enum CodingKeys: String, CodingKey {
        case severity, title, path, symbol, message
        case fixPrompt = "fix_prompt"
    }
}

/// Per-file lint result set.
struct HalDocLintFile: Codable {
    var path: String
    var issues: [HalDocLintIssue]
}

/// Full lint report.
struct HalDocLintReport: Codable {
    var files: [HalDocLintFile]
}


/// Mirrors the Rust `cpp_syntax_check` payload: structural C++ validity.
struct CppSyntaxError: Codable {
    var line: Int
    var col: Int
    var kind: String
    var context: String
}

struct CppSyntaxReport: Codable {
    var ok: Bool
    var errors: [CppSyntaxError]
}


/// Mirrors the Rust `hal_state` snapshot (contract + per-impl states).
struct HalContractState: Codable {
    var state: String
    var errors: Int?
    var issues: Int?
}

struct HalImplState: Codable {
    var state: String
    var missing: [String]?
    var drifted: [String]?
}

struct HalImplRow: Codable, Identifiable {
    var id: String { platform }
    var platform: String
    var state: HalImplState
    var contracts: [String]
}

struct HalStateSnapshot: Codable {
    var contract: HalContractState
    var implementations: [HalImplRow]
    var lintIssues: Int
    var syntaxErrors: Int

    enum CodingKeys: String, CodingKey {
        case contract, implementations
        case lintIssues = "lint_issues"
        case syntaxErrors = "syntax_errors"
    }
}


/// Whole-file prompt result for one HAL header.
struct HalFixPromptResult: Codable {
    var issues: [HalDocLintIssue]
    var prompt: String
}


/// Proposal result from the coordinator (whole-file LLM rewrite).
struct HalFixProposeResult: Codable {
    var status: String   // "proposed" | "clean" | "error"
    var path: String?
    var proposedContent: String?
    var issues: [HalDocLintIssue]?
    var error: String?

    enum CodingKeys: String, CodingKey {
        case status, path
        case proposedContent = "proposed_content"
        case issues, error
    }
}
