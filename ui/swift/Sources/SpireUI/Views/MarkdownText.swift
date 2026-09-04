import SwiftUI

/// Renders markdown as real blocks: headings, bullet/numbered lists, fenced
/// code blocks and paragraphs each on their own line.
///
/// Foundation's `AttributedString(markdown:)` parses these fine but strips the
/// block-level newlines out of the character stream (they become
/// `presentationIntent`s that SwiftUI `Text` does not lay out), so the whole
/// answer collapses into one wrapped paragraph. We therefore split the
/// markdown into blocks ourselves and hand the parser only a single block at a
/// time, where it can only apply inline styling (bold/italic/code/links).
struct MarkdownText: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(Self.blocks(from: text).enumerated()), id: \.offset) { _, block in
                blockView(block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func blockView(_ block: Block) -> some View {
        switch block.kind {
        case .heading(let level):
            Text(inline(block.text))
                .font(Self.headingFont(level))
        case .code:
            Text(codeText(block.text))
                .font(.system(.caption, design: .monospaced))
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.45)))
        case .bullet:
            HStack(alignment: .top, spacing: 6) {
                Text("•").foregroundStyle(.secondary)
                Text(inline(block.text))
            }
        case .numbered(let n):
            HStack(alignment: .top, spacing: 6) {
                Text("\(n).").foregroundStyle(.secondary).monospacedDigit()
                Text(inline(block.text))
            }
        case .paragraph:
            Text(inline(block.text))
        }
    }

    /// Inline markdown only — safe here because a single block contains no
    /// block-level breaks the parser could drop.
    private func inline(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s)) ?? AttributedString(s)
    }

    private func codeText(_ s: String) -> String {
        var t = s
        while t.hasSuffix("\n") { t.removeLast() }
        return t
    }

    private struct Block {
        enum Kind {
            case heading(Int)
            case code
            case bullet
            case numbered(Int)
            case paragraph
        }
        let kind: Kind
        let text: String
    }

    private static func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: return .title2.weight(.semibold)
        case 2: return .title3.weight(.semibold)
        default: return .headline.weight(.semibold)
        }
    }

    private static func blocks(from raw: String) -> [Block] {
        let lines = raw.components(separatedBy: "\n")
        var out: [Block] = []
        var i = 0
        while i < lines.count {
            let line = lines[i].trimmingCharacters(in: .whitespaces)
            if line.isEmpty {
                i += 1
                continue
            }
            // Fenced code block (blank lines inside are preserved).
            if line.hasPrefix("```") {
                var code: [String] = []
                i += 1
                while i < lines.count && !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                    code.append(lines[i])
                    i += 1
                }
                if i < lines.count { i += 1 } // skip the closing fence
                if !code.isEmpty {
                    out.append(Block(kind: .code, text: code.joined(separator: "\n")))
                }
                continue
            }
            // ATX heading.
            if let level = headingLevel(line) {
                let body = stripClosingHashes(String(line.dropFirst(level)))
                if !body.isEmpty {
                    out.append(Block(kind: .heading(level), text: body))
                }
                i += 1
                continue
            }
            // Bullet list (contiguous lines).
            if bulletItem(line) != nil {
                while i < lines.count, let item = bulletItem(lines[i].trimmingCharacters(in: .whitespaces)) {
                    out.append(Block(kind: .bullet, text: item))
                    i += 1
                }
                continue
            }
            // Numbered list (contiguous lines).
            if numberedItem(line) != nil {
                var items: [String] = []
                while i < lines.count, let item = numberedItem(lines[i].trimmingCharacters(in: .whitespaces)) {
                    items.append(item)
                    i += 1
                }
                for (idx, item) in items.enumerated() {
                    out.append(Block(kind: .numbered(idx + 1), text: item))
                }
                continue
            }
            // Paragraph: collect until a blank or structural line.
            var para: [String] = []
            while i < lines.count {
                let t = lines[i]
                let tr = t.trimmingCharacters(in: .whitespaces)
                if tr.isEmpty || tr.hasPrefix("```")
                    || headingLevel(tr) != nil || bulletItem(tr) != nil || numberedItem(tr) != nil {
                    break
                }
                para.append(t)
                i += 1
            }
            if !para.isEmpty {
                out.append(Block(kind: .paragraph, text: para.joined(separator: "\n")))
            }
        }
        return out
    }

    private static func headingLevel(_ s: String) -> Int? {
        var level = 0
        for ch in s {
            if ch == "#" { level += 1 } else { break }
        }
        guard level > 0, level <= 6 else { return nil }
        let rest = s.dropFirst(level)
        guard rest.isEmpty || rest.first == " " else { return nil }
        return level
    }

    private static func stripClosingHashes(_ s: String) -> String {
        var t = s.trimmingCharacters(in: .whitespaces)
        while t.hasSuffix("#") { t = String(t.dropLast()).trimmingCharacters(in: .whitespaces) }
        return t
    }

    /// Returns the text after a bullet marker (`- `, `* `, `+ `) if any.
    private static func bulletItem(_ s: String) -> String? {
        for marker in ["- ", "* ", "+ "] where s.hasPrefix(marker) {
            return String(s.dropFirst(marker.count)).trimmingCharacters(in: .whitespaces)
        }
        return nil
    }

    /// Returns the text after a numbered marker (`1. `, `2) `) if any.
    private static func numberedItem(_ s: String) -> String? {
        var digits = ""
        for ch in s {
            if ch.isNumber { digits.append(ch) } else { break }
        }
        guard !digits.isEmpty else { return nil }
        let rest = String(s.dropFirst(digits.count))
        guard rest.hasPrefix(". ") || rest.hasPrefix(") ") || rest == "." || rest == ")" else { return nil }
        return String(rest.dropFirst(rest == "." || rest == ")" ? 1 : 2)).trimmingCharacters(in: .whitespaces)
    }
}
