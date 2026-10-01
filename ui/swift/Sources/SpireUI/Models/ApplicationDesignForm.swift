import Foundation

/// The **board** the design form asks for.
///
/// A board is required rather than defaulted, and the Rust side says why: a guessed chip or BSP is a
/// build that fails on hardware. `chip` and `bsp` are what it insists on; the abstraction (`hal`) is
/// optional, because plenty of boards have none.
struct BoardChoice: Equatable {
    var chip = ""
    var bsp = ""
    var hal = ""

    private static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// A board a build can be made for. The same rule the core applies, checked here so the button is
    /// disabled rather than the design refused.
    ///
    /// **A chip is required; a BSP is not.** The chip decides the compile target and a guess at it is a
    /// build that fails on hardware; a BSP is a manifest key that most boards simply do not have — the
    /// catalogue carries one on one board in ten — and an empty one means *Spire generates its own
    /// backend*, which is an answer rather than a gap. Requiring it was how a board with none ended up
    /// hand-typed, and a hand-typed BSP is a manifest key nobody validated.
    var isComplete: Bool {
        !Self.trimmed(chip).isEmpty
    }

    var json: [String: Any] {
        var out: [String: Any] = ["chip": Self.trimmed(chip), "bsp": Self.trimmed(bsp)]
        let abstraction = Self.trimmed(hal)
        if !abstraction.isEmpty { out["hal"] = abstraction }
        return out
    }
}

/// The design form's **six answers**, in the person's own words.
///
/// They travel to the model as one `description`, each answer labelled with its question. The labels are
/// the point: the design request asks the model to say which question went unanswered rather than invent
/// a device, an address or a protocol to fill the gap, and a model handed six labelled paragraphs can see
/// the gap. An empty answer is left out — a blank line under a heading is not an answer, and sending it
/// would make the gap look filled.
struct ApplicationDesignForm: Equatable {
    var purpose = ""
    var senses = ""
    var actsOn = ""
    var reactsTo = ""
    var timing = ""
    var missing = ""

    /// The questions, in the order the design request asks them.
    static let questions = [
        "what it does",
        "what it senses",
        "what it acts on",
        "what it reacts to over time",
        "timing",
        "what happens when something is missing",
    ]

    /// The answers as `(question, answer)`, in request order.
    var answers: [(question: String, answer: String)] {
        let given = [purpose, senses, actsOn, reactsTo, timing, missing]
        return zip(Self.questions, given).map { question, answer in
            (question, answer.trimmingCharacters(in: .whitespacesAndNewlines))
        }
    }

    var description: String {
        answers
            .filter { !$0.answer.isEmpty }
            .map { question, answer in
                // Sentence case, not `capitalized`: that would title-case every word ("What It Does"),
                // and these labels are read as sentences.
                "\(question.prefix(1).uppercased() + question.dropFirst()): \(answer)"
            }
            .joined(separator: "\n\n")
    }

    /// Whether there is anything to design from at all.
    var isUsable: Bool { !description.isEmpty }
}
