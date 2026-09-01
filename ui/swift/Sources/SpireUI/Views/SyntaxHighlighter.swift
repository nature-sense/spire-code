import SwiftUI

/// Pure-Swift syntax highlighter (no C dependencies -> cannot crash).
/// Uses `AttributedString` with regex-based token colours for the languages
/// Spire scaffolds: Python, Rust, Swift, JS, Go, Java, Ruby + TOML/JSON/C++.
enum SyntaxLanguage {
    case python, rust, swift, javascript, go, java, ruby
    case toml, json, cpp, meson, plain

    /// Detect language from a file path/extension (case-insensitive).
    static func detect(from path: String) -> SyntaxLanguage {
        let ext = (path as NSString).pathExtension.lowercased()
        switch ext {
        case "py", "pyw": return .python
        case "rs": return .rust
        case "swift": return .swift
        case "js", "jsx", "ts", "tsx", "mjs": return .javascript
        case "go": return .go
        case "java": return .java
        case "rb", "rake", "gemspec": return .ruby
        case "toml": return .toml
        case "json", "jsonc": return .json
        case "c", "h", "cpp", "hpp", "cc", "cxx": return .cpp
        case "meson": return .meson
        default:
            let name = (path as NSString).lastPathComponent.lowercased()
            switch name {
            case "cargo.toml", "pyproject.toml": return .toml
            case "package.json", "tsconfig.json": return .json
            case "gemfile", "rakefile": return .ruby
            default: return .plain
            }
        }
    }

    /// Reserved words per language.
    fileprivate var keywords: Set<String> {
        switch self {
        case .python:
            return ["def","class","if","elif","else","for","while","return","import","from",
                    "as","try","except","finally","with","in","is","not","and","or","None",
                    "True","False","raise","pass","break","continue","lambda","del","yield",
                    "async","await","global","nonlocal","assert"]
        case .rust:
            return ["fn","let","mut","const","static","struct","enum","trait","impl","use","mod",
                    "pub","crate","self","super","match","if","else","for","while","loop","return",
                    "break","continue","move","ref","type","where","async","await","unsafe","true","false"]
        case .swift:
            return ["func","var","let","class","struct","enum","protocol","actor","extension","import",
                    "init","if","else","guard","switch","case","for","in","while","return","break","continue",
                    "self","super","static","final","override","public","private","internal","fileprivate",
                    "open","async","await","throws","true","false","nil"]
        case .javascript:
            return ["function","const","let","var","class","extends","return","if","else","for","while",
                    "do","switch","case","break","continue","new","typeof","instanceof","in","of","import",
                    "export","from","default","async","await","yield","try","catch","finally","throw",
                    "this","super","null","undefined","true","false"]
        case .go:
            return ["func","var","const","type","struct","interface","map","package","import","defer",
                    "go","chan","select","switch","case","if","else","for","range","return","break",
                    "continue","default","nil","true","false"]
        case .java:
            return ["public","private","protected","class","interface","enum","extends","implements",
                    "static","final","void","int","long","double","float","boolean","char","byte","new",
                    "return","if","else","for","while","do","switch","case","try","catch","finally","throw",
                    "throws","package","import","this","super","true","false","null"]
        case .ruby:
            return ["def","class","module","if","elsif","else","unless","case","when","while","until",
                    "for","in","do","end","return","break","next","begin","rescue","ensure","raise","yield",
                    "lambda","require","require_relative","nil","true","false","self","super"]
        default: return []
        }
    }

    /// Line-comment marker(s) for the language.
    fileprivate var lineComments: [String] {
        switch self {
        case .python, .ruby: return ["#"]
        case .swift, .go, .java, .javascript, .rust, .cpp: return ["//"]
        default: return ["#"]
        }
    }
}

enum SyntaxHighlighter {
    static func highlight(_ code: String, language: SyntaxLanguage) -> AttributedString {
        var attr = AttributedString(code)
        // Work in UTF-16 offsets (what NSRange/NSRegularExpression use).
        let ns = code as NSString
        let lines = code.components(separatedBy: "\n")
        var location = 0

        for line in lines {
            let lineLen = (line as NSString).length

            // Comment colour (green) — colour the span after the marker, only if
            // the marker appears outside a string literal.
            if let cm = language.lineComments.first {
                let cmNS = cm as NSString
                let text = line as NSString
                let marker = text.range(of: cm)

                func isInString(_ loc: Int) -> Bool {
                    let prefix = text.substring(to: loc) as NSString
                    let quotes = prefix.range(of: "\"", options: [.backwards]).location
                    let single = prefix.range(of: "'", options: [.backwards]).location
                    // Naive but suitable for line-based highlighting: a quote
                    // before the marker with no unpaired counterpart on the line.
                    return max(quotes, single) != NSNotFound
                }

                if marker.location != NSNotFound, !isInString(marker.location) {
                    let commentLength = lineLen - (marker.location + cmNS.length)
                    if commentLength > 0 {
                        let nsRng = NSRange(location: location + marker.location + cmNS.length,
                                            length: commentLength)
                        if let r = Range(nsRng, in: attr) {
                            attr[r].foregroundColor = .green
                        }
                    }
                }
            }

            // Keywords (blue) + string literals (red) via regex over this line.
            let regex = try? NSRegularExpression(
                pattern: "\\b([A-Za-z_][A-Za-z0-9_]*)\\b|(\"[^\"]*\"|'[^']*')"
            )
            for match in regex?.matches(in: line, range: NSRange(location: 0, length: lineLen)) ?? [] {
                let wordRange = match.range(at: 1)
                if wordRange.location != NSNotFound {
                    let word = (line as NSString).substring(with: wordRange)
                    if language.keywords.contains(word) {
                        let nsRng = NSRange(location: location + wordRange.location, length: wordRange.length)
                        if let r = Range(nsRng, in: attr) {
                            attr[r].foregroundColor = .blue
                        }
                    }
                }
                let strRange = match.range(at: 2)
                if strRange.location != NSNotFound {
                    let nsRng = NSRange(location: location + strRange.location, length: strRange.length)
                    if let r = Range(nsRng, in: attr) {
                        attr[r].foregroundColor = .red
                    }
                }
            }

            // Advance past this line and its trailing newline (if any).
            location += lineLen + 1
        }
        return attr
    }
}