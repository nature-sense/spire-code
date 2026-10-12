import Foundation

/// The **design brief** — what a description says the application has to do, before a board is chosen.
///
/// Mirror of the Rust `DesignBrief`, read over `createProject/BriefApplication`. It is the
/// description-first flow's first step's answer, and it stops deliberately short of an
/// [`ApplicationDesign`]: it names the capabilities the application needs (`requires`), the framework
/// the description points at, the units it expects (`suggests`) and the description **read against the
/// six points** (`points`) — and never a board, a resource, a driver or a wiring edge.
///
/// It is a **guide, not a gate**: `requires` drives the `Needed` badges, the board-fit readout and the
/// uncovered warning, and the framework is a suggestion the person confirms when they choose a board.
/// Nothing here disables Continue.
///
/// **The model drafts; the person edits.** Where a point is left open the brief does not ask — it
/// decides, and says so in `decided` — and the one thing it does hand back as a question is a `forks`
/// entry: a *pick* between named alternatives that carry the capabilities each would add. So answering
/// it is choosing the paths, rather than writing prose the model has to read all over again.
struct DesignBrief: Codable, Hashable {
    /// The application in a paragraph, in the model's words, for the person to confirm.
    var summary: String
    /// The framework the description points at (`actors`/`ramen`), or `nil` when it was too thin to
    /// tell — an answer rather than a blank, because the person chooses the framework at the board step.
    var framework: String?
    /// The **capabilities** the application needs, as paths the vocabulary knows — the core checked
    /// each one before it sent the brief, so every path here is a path the rest of the system knows.
    var requires: [String]
    /// The **units** the description implies, as advisory lower_snake names for the actor step to start
    /// from. Names only: no kind, no message, no wiring.
    var suggests: [String]
    /// The description **read against the six points** ([`BriefPoint`]) — one entry per point the model
    /// actually read. The card draws all six rows either way, so a point missing from here is a **blank
    /// row under a label**, and that is the whole difference from the old `gaps` list: what is open is
    /// legible point by point rather than a count that moved between two reads of the same text.
    var points: [BriefPointRead]
    /// The decisions the brief could not settle and would not guess — **at most two**, each a pick
    /// between named alternatives that carry the capabilities they would add. Empty is the ordinary
    /// answer: a fork exists only where a guess would change what the application is *given*.
    var forks: [BriefFork]

    init(
        summary: String = "",
        framework: String? = nil,
        requires: [String] = [],
        suggests: [String] = [],
        points: [BriefPointRead] = [],
        forks: [BriefFork] = []
    ) {
        self.summary = summary
        self.framework = framework
        self.requires = requires
        self.suggests = suggests
        self.points = points
        self.forks = forks
    }

    enum CodingKeys: String, CodingKey {
        case summary, framework, requires, suggests, points, forks
    }

    /// Tolerant on purpose: a brief is a model's answer, so a missing key is *nothing said* rather than
    /// a decode failure. An absent field is an answer the flow handles (a description too thin to place,
    /// a brief that named no units); a thrown error would be a dead step with no way forward.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        summary = (try? container.decode(String.self, forKey: .summary)) ?? ""
        framework = try? container.decodeIfPresent(String.self, forKey: .framework)
        requires = (try? container.decode([String].self, forKey: .requires)) ?? []
        suggests = (try? container.decode([String].self, forKey: .suggests)) ?? []
        points = (try? container.decode([BriefPointRead].self, forKey: .points)) ?? []
        forks = (try? container.decode([BriefFork].self, forKey: .forks)) ?? []
    }

    /// Whether the brief says anything at all — the read the view uses to decide whether to draw its
    /// card, so an empty brief is silent rather than an empty box.
    var isEmpty: Bool {
        summary.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && requires.isEmpty && suggests.isEmpty
            && points.isEmpty && forks.isEmpty && framework == nil
    }

    /// The capabilities as one line, for a card that shows what the application needs.
    var requiresLabel: String { requires.joined(separator: ", ") }

    /// How one of the six points was read — `nil` when the model did not read it at all, which the card
    /// draws as a blank row rather than by hiding the point.
    func reading(of point: BriefPoint) -> BriefPointRead? {
        points.first { $0.point == point.rawValue }
    }
}

/// The **six points** a description is read against — the mirror of the Rust `BRIEF_POINTS`.
///
/// `allCases` is what the describe step's card iterates, so the six rows are drawn from this list and
/// never from what the model happened to return. That is the structural half of the fix for a brief
/// whose length moved between reads: there is no length on screen to move. A point the model skipped is
/// a row whose reading is empty, which says *exactly* which question went unanswered — where the old
/// `gaps` list simply got shorter and said nothing at all about which part of the description it had
/// given up on.
enum BriefPoint: String, CaseIterable, Identifiable {
    case purpose
    case senses
    case actsOn = "acts_on"
    case reactsTo = "reacts_to"
    case timing
    case missing

    var id: String { rawValue }

    /// The point as a row label — the question in as few words as a row can carry, since the full one,
    /// with its examples, is the request's business rather than the card's.
    var label: String {
        switch self {
        case .purpose: return "What it does"
        case .senses: return "What it senses"
        case .actsOn: return "What it acts on"
        case .reactsTo: return "What it reacts to"
        case .timing: return "Timing"
        case .missing: return "When something is missing"
        }
    }
}

/// One point of the brief's reading: **what the description said** about it, and **what was decided**
/// where it said nothing.
///
/// Two cells rather than one, and that is the shape of the whole change. `said` is the model's reading of
/// the person's own words, checkable against the text above; `decided` is its draft — the answer it
/// supplied so the design could carry on instead of handing the person a question. A row with both cells
/// filled is a description that was partly there, a row with `decided` alone is a point the model carried
/// by guessing, and a row with neither is a point nobody read — which is visible as such.
struct BriefPointRead: Codable, Hashable {
    var point: String
    var said: String
    var decided: String

    init(point: String = "", said: String = "", decided: String = "") {
        self.point = point
        self.said = said
        self.decided = decided
    }

    enum CodingKeys: String, CodingKey { case point, said, decided }

    /// Tolerant like the brief itself: a missing key is *nothing said* rather than a decode failure, so a
    /// brief that abbreviates one cell does not throw the whole step away.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        point = (try? container.decode(String.self, forKey: .point)) ?? ""
        said = (try? container.decode(String.self, forKey: .said)) ?? ""
        decided = (try? container.decode(String.self, forKey: .decided)) ?? ""
    }

    /// Whether the model had nothing to say about this point at all.
    var isBlank: Bool {
        said.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && decided.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

/// A decision the brief could not settle from the description **and** would not settle alone, because
/// the answer changes which capabilities or units the application is given.
///
/// This is the only question left in a brief, and it is answered by **picking**, not by writing: each
/// choice carries the `requires` and `suggests` it would add, so the person edits the structure and the
/// later steps read it. The core caps these at two and checks every path inside them, so a choice shown
/// here is one whose capabilities the rest of the system knows.
struct BriefFork: Codable, Hashable {
    var point: String
    var question: String
    var choices: [BriefChoice]

    init(point: String = "", question: String = "", choices: [BriefChoice] = []) {
        self.point = point
        self.question = question
        self.choices = choices
    }

    enum CodingKeys: String, CodingKey { case point, question, choices }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        point = (try? container.decode(String.self, forKey: .point)) ?? ""
        question = (try? container.decode(String.self, forKey: .question)) ?? ""
        choices = (try? container.decode([BriefChoice].self, forKey: .choices)) ?? []
    }

    /// The point this decision belongs to, when it names one of the six — for the row the buttons sit
    /// under. `nil` for an id the card has no label for, which is then shown as the id itself.
    var briefPoint: BriefPoint? { BriefPoint(rawValue: point) }
}

/// One answer to a [`BriefFork`]: a short name for the alternative and **what it would add** — the
/// capability paths and unit names true of this answer and not of the others.
///
/// The paths are what make a fork answerable in one click instead of in prose, so picking a choice adds
/// them to the brief's `requires`/`suggests` and everything downstream — the `Needed` badges, the
/// board-fit readout, the units the actor step starts from — follows without a word being written.
struct BriefChoice: Codable, Hashable {
    var label: String
    var requires: [String]
    var suggests: [String]

    init(label: String = "", requires: [String] = [], suggests: [String] = []) {
        self.label = label
        self.requires = requires
        self.suggests = suggests
    }

    enum CodingKeys: String, CodingKey { case label, requires, suggests }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        label = (try? container.decode(String.self, forKey: .label)) ?? ""
        requires = (try? container.decode([String].self, forKey: .requires)) ?? []
        suggests = (try? container.decode([String].self, forKey: .suggests)) ?? []
    }

    /// What picking this one adds, for the line under the buttons — `storage.sd, logger`.
    var additionLabel: String { (requires + suggests).joined(separator: ", ") }
}
