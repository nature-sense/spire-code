import Foundation
import AppKit
import os
import os
import Observation

/// Central observable state object that communicates with the Rust core.
@Observable
final class SpireBridge {
    /// Shared singleton used by the macOS menu-bar commands so they can
    /// reference the exact same instance as the SwiftUI environment.
    static let shared = SpireBridge()

    /// Single source of truth for the UI's project state machine.
    var state: ProjectState = .unconnected

    // MARK: Derived computed properties (kept for backward compatibility
    // with views that read the old booleans; views should migrate to `state`).

    var connected: Bool {
        if case .idle = state { return true }
        return false
    }
    var connectionError: String? {
        if case .error(let m) = state { return m }
        return nil
    }
    var projectInfo: ProjectInfo? {
        if case .idle(let p) = state { return p }
        return nil
    }
    var loading: Bool {
        if case .opening = state { return true }
        return false
    }
    var showWelcome: Bool {
        if case .unconnected = state { return true }
        return false
    }
    var showNewProject: Bool {
        if case .creating = state { return true }
        return false
    }
    var creationPlan: PlanGenerationResult? {
        if case .creating(let plan, _) = state { return plan }
        return nil
    }
    var creationExecuting: Bool {
        if case .creating(_, let exec) = state { return exec }
        return false
    }

    var messages: [ChatMessage] = []
    /// Last plan-creation error from the backend (nil on success).
    var lastPlanError: String?
    /// The latest generated (or approved/executing) plan. Promoted from
    /// PlanSheetView so the right pane's collapsible task list can render the
    /// plan steps live (status updates after approve/execute).
    var activePlan: PlanStatusResult?
    /// Whether the plan sheet/task-list should be expanded/visible.
    var planVisible = false
    var isProcessing: Bool = false
    var selectedSubproject: SubprojectInfo?
    var selectedBuildTarget: String?
    /// Selected HAL domain (nil = the project root). Set by the ProjectAnalysisView
    /// Domains card; drives the far-right Action panel's scope/platform.
    var selectedDomain: String?
    /// When the layout's `hal/api` row is clicked, the right-pane HAL contract
    /// lint ("Validate Contract Format" correction plan) becomes visible.
    /// Cleared when any other layout leaf is selected.
    var showHalContractLint: Bool = false
    /// Sum of contract-format lint issues across all hal/api headers (0 = OK).
    /// Populated by every `halDocLint` call so the right-pane implementation
    /// action gate stays in sync with the api row badge.
    var halContractIssueCount: Int = 0
    /// True when all HAL contracts pass the format lint (no issues).
    var halContractsValid: Bool { halContractIssueCount == 0 }
    var currentMode: SpireMode = .project
    var showChat: Bool = false
    /// The directory of the currently opened project.
    var projectRoot: String?

    // MARK: - HAL status (module list + per-platform gaps)
    // Refreshed by `refreshHalData` on every project open/analysis so the left
    // "Platforms" card can show "HAL N modules" / per-platform "HAL OK" or
    // "HAL N modules missing" without re-querying the core per view.
    /// HAL contract/module stems (from `hal_sanity_check` → interfaces).
    var halInterfaces: [String] = []
    /// platformId → incomplete HAL interface names (from the AST coverage
    /// re-computed fresh from disk in `hal_missing_impls` → platforms map).
    var halMissingByPlatform: [String: [String]] = [:]
    /// platformId → interface → { implemented, missing: [function names],
    /// drifted: [signature-change messages] }. Populated by `refreshHalData`
    /// from the richer `hal_missing_impls` payload; drives the right-pane
    /// "Add missing HAL functions" action's concrete LLM input.
    var halFunctionGaps: [String: [String: HalInterfaceGaps]] = [:]
    /// One interface's function-level gaps for a platform.
    struct HalInterfaceGaps: Codable, Hashable {
        let implemented: Bool
        /// True when a stem-matching impl file exists (vs none at all).
        let has_impl: Bool
        /// True when the impl file carries `SPIRE-HAL-STUB` (generated
        /// placeholder, not a real implementation).
        let is_stub: Bool
        let missing: [String]
        let drifted: [String]

        /// Maturity state: implemented / stub / partial / missing.
        var maturity: String {
            if implemented { return "implemented" }
            if is_stub { return "stub" }
            if has_impl { return "partial" }
            return "missing"
        }
    }

    /// Refresh the HAL module list + per-platform AST coverage for a project
    /// root. Called from `openProject`/`fetchProjectAnalysis` so the indicators
    /// stay in sync whenever analysis is (re)run. The coverage is computed
    /// fresh from disk (contract pure-virtual method set vs each platform's
    /// out-of-class definitions), so it reflects the CURRENT sources.
    func refreshHalData(root: String) async {
        let (report, _) = await halSanityCheck(root: root)
        // hal_missing_impls now returns { missing, platforms } where `platforms`
        // maps platformId → interface → { implemented, missing, drifted }.
        // JSONSerialization yields [String: Any] at every level, so parse
        // step-by-step (nested `as? [String: ...]` through `Any` is unreliable).
        let (payload, _) = await halMissingImpls(root: root)
        var byPlatform: [String: [String]] = [:]
        var functionGaps: [String: [String: HalInterfaceGaps]] = [:]

        // 1. per-interface missing platform lists (interface → [platforms]).
        if let missing = payload["missing"] as? [String: Any] {
            for (iface, platsAny) in missing {
                guard let plats = platsAny as? [String] else { continue }
                for p in plats {
                    byPlatform[p, default: []].append(iface)
                }
            }
        }

        // 2. per-platform × interface function gaps.
        if let platformsWrapper = payload["platforms"] as? [String: Any] {
            for (plat, interfacesAny) in platformsWrapper {
                guard let interfaces = interfacesAny as? [String: Any] else { continue }
                var ifaceMap: [String: HalInterfaceGaps] = [:]
                for (iface, infoAny) in interfaces {
                    guard let info = infoAny as? [String: Any] else { continue }
                    let implemented = info["implemented"] as? Bool ?? false
                    let hasImpl = info["has_impl"] as? Bool ?? false
                    let isStub = info["is_stub"] as? Bool ?? false
                    let missingFns = info["missing"] as? [String] ?? []
                    let drifted = info["drifted"] as? [String] ?? []
                    ifaceMap[iface] = HalInterfaceGaps(implemented: implemented,
                                                       has_impl: hasImpl,
                                                       is_stub: isStub,
                                                       missing: missingFns,
                                                       drifted: drifted)
                }
                functionGaps[plat] = ifaceMap
            }
        }

        await MainActor.run {
            self.halInterfaces = report?.interfaces ?? []
            self.halMissingByPlatform = byPlatform
            self.halFunctionGaps = functionGaps
        }
    }

    // MCP servers state
    var mcpServers: [McpServerInfo] = []
    var mcpServersLoading: Bool = false
    var mcpServersError: String?
    /// Per-server cached tools: serverName → [McpToolInfo]
    var mcpTools: [String: [McpToolInfo]] = [:]
    /// Per-server loading state for tools
    var mcpToolsLoading: Set<String> = []
    /// All tools from all backends (via tools/list)
    var allTools: [McpToolInfo] = []
    /// True while fetching all tools
    var allToolsLoading: Bool = false

    // LLM settings state
    var llmConfig: LLMConfig = LLMConfig()
    var llmConfigLoading: Bool = false

    // Recent projects state
    var recentProjects: [RecentProject] = []

    /// The latest scaffold spec returned by `createProject/Scaffold` — exposed
    /// so the wizard's structure preview + fill step can read it.
    var scaffoldSpec: ScaffoldSpec?

    private let backend: any UIBackend

    /// Create a BuildService backed by this bridge's UIBackend. The service owns
    /// all build/lint/fix calls + the single live-event consumer (actor-isolated).
    func makeBuildService() -> BuildService {
        BuildService(backend: backend)
    }

    /// Create a ProjectService backed by this bridge's UIBackend. The service
    /// owns all project analyze/open/readFile FFI calls (repository pattern).
    func makeProjectService() -> ProjectService {
        ProjectService(backend: backend)
    }

    /// Fetch target-scoped detail (deps/platform/files) for a build target
    /// from the knowledge graph via `project/getBuildTarget`.
    func fetchBuildTarget(name: String) async throws -> BuildTargetDetail {
        try await makeProjectService().fetchBuildTarget(name: name)
    }

    /// Create an LLMService backed by this bridge's UIBackend. The service owns
    /// all LLM config FFI calls (config/getAll, config/set).
    func makeLLMService() -> LLMService {
        LLMService(backend: backend)
    }

    var showSettings: Bool = false

    private var menuObservers: [NSObjectProtocol] = []

    init(backend: any UIBackend = SpireFFIBackend()) {
        self.backend = backend

        let nc = NotificationCenter.default
        self.menuObservers = [
            nc.addObserver(forName: MenuCommand.refreshProject, object: nil, queue: .main) { [weak self] _ in
                Task { await self?.fetchProjectAnalysis() }
            },
            nc.addObserver(forName: MenuCommand.toggleChat, object: nil, queue: .main) { [weak self] _ in
                self?.showChat.toggle()
            },
            nc.addObserver(forName: MenuCommand.showSettings, object: nil, queue: .main) { [weak self] _ in
                self?.showSettings = true
            },
            nc.addObserver(forName: MenuCommand.newProject, object: nil, queue: .main) { [weak self] _ in
                guard let self else { return }
                self.closeProject()
                self.state = .creating(plan: nil, executing: false)
                self.currentMode = .project
            },
        ]
    }

    // MARK: - Connectivity check

    /// True when the Rust core (dylib) is reachable. Used to surface a
    /// clear, actionable error at startup instead of a generic failure
    /// when the core hasn't been built.
    private(set) var coreAvailable: Bool = false

    /// Verify the Rust core is reachable. Called on startup; if the core
    /// cannot be contacted the bridge transitions to `.error` with an
    /// actionable message so the user knows to build the Rust side.
    func checkConnection() async {
        guard backend.isAvailable else {
            state = .error(
                "Rust core not available. Please build it with `make rust` " +
                "or `cargo build --release -p spire-code`, then relaunch the app."
            )
            return
        }

        do {
            let body: [String: Any] = ["method": "status", "params": [:]]
            let data = try JSONSerialization.data(withJSONObject: body)
            _ = try await backend.send(data)
            coreAvailable = true
        } catch {
            state = .error(
                "Could not reach the Rust core: \(error.localizedDescription). " +
                "Please ensure the core is built and relaunch."
            )
        }
    }

    // MARK: - Commands

    /// Fetch the full project analysis from the Rust core.
    ///
    /// Always passes an explicit `root` to the core so re-analysis is a fresh
    /// disk scan (never a stale cached analysis). Falls back to the last known
    /// project root when none is supplied.
    func fetchProjectAnalysis(projectRoot: String? = nil) async {
        state = .opening

        // Authoritative root: explicit, else last known opened/analyzed root.
        let root = projectRoot ?? self.projectRoot

        do {
            let body: [String: Any] = [
                "method": "AnalyzeProject",
                "params": rootParams(root)
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let decoded: ProjectInfo = try MessageSerializer.decode(reply)
            // DIAGNOSTIC: log exactly what the Swift decode produced so the
            // subproject pipeline can be localized (decode drop vs render).
            // This is the REFRESH / menu path — the dashboard is loaded here.
            NSLog("[Bridge] fetchProjectAnalysis root=%@ subprojects=%d", decoded.root, decoded.subprojects.count)
            for s in decoded.subprojects {
                NSLog("[Bridge]   sub=%@ path=%@ files=%d", s.name, s.path, s.files?.count ?? -1)
            }
            let resolvedRoot = decoded.root
            await MainActor.run {
                self.projectRoot = resolvedRoot
                // MUST be executed on the main actor — @Observable does not
                // reliably invalidate views for off-main writes (this left the
                // app stuck on "Opening project…" even though the core returned
                // in <100ms).
                self.state = .idle(decoded)
            }
            // HAL projects: refresh the module list + per-platform gaps so the
            // left "Platforms" card indicators stay in sync with re-analysis.
            if decoded.subprojects.contains(where: { $0.structure == "hal" }) {
                await refreshHalData(root: resolvedRoot)
            }
        } catch {
            let msg = "Error: \(error.localizedDescription)"
            Logger(subsystem: "spire", category: "startup").error("fetchProjectAnalysis failed: \(String(describing: error))")
            await MainActor.run { self.state = .error(msg) }
        }
    }

    /// Open a project directory (creating it if needed). Initializes the
    /// graph database for the project, runs analysis, and returns ProjectInfo.
    func openProject(root: String) async {
        Logger(subsystem: "spire", category: "menu").info("openProject called with root=\(root)")
        state = .opening

        do {
            let body: [String: Any] = [
                "method": "project/open",
                "params": ["root": root]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            Logger(subsystem: "spire", category: "menu").info("openProject sending project/open for \(root)")
            let t0 = Date()
            let reply = try await backend.send(data)
            NSLog("[OpenTiming] backend.send returned after %.3fs replyBytes=%d", Date().timeIntervalSince(t0), reply.count)
            let t1 = Date()
            let decoded: ProjectInfo = try MessageSerializer.decode(reply)
            NSLog("[OpenTiming] MessageSerializer.decode returned after %.3fs subprojects=%d", Date().timeIntervalSince(t1), decoded.subprojects.count)
            Logger(subsystem: "spire", category: "menu").info("openProject succeeded: \(decoded.name) at \(decoded.root)")
            // DIAGNOSTIC: log exactly what the Swift decode produced so the
            // subproject pipeline can be localized (decode drop vs render).
            NSLog("[Bridge] openProject root=%@ subprojects=%d", decoded.root, decoded.subprojects.count)
            for s in decoded.subprojects {
                NSLog("[Bridge]   sub=%@ path=%@ files=%d", s.name, s.path, s.files?.count ?? -1)
            }
            await MainActor.run {
                self.projectRoot = decoded.root
                currentMode = .project
                RecentProject.record(path: decoded.root, name: decoded.name)
                recentProjects = RecentProject.load()
                NSLog("[OpenTiming] .idle state set after %.3fs total", Date().timeIntervalSince(t0))
                // MUST be executed on the main actor — @Observable does not
                // reliably invalidate views for off-main writes (this left the
                // app stuck on "Opening project…" even though the core returned
                // in <100ms).
                state = .idle(decoded)
            }
            // HAL projects: refresh the module list + per-platform gaps so the
            // left "Platforms" card indicators update on open.
            if decoded.subprojects.contains(where: { $0.structure == "hal" }) {
                await refreshHalData(root: decoded.root)
            }
        } catch {
            let msg = "Error: \(error.localizedDescription)"
            Logger(subsystem: "spire", category: "startup").error("openProject failed: \(String(describing: error))")
            await MainActor.run { state = .error(msg) }
        }
    }

    /// Load the list of recently opened projects from disk.
    func loadRecentProjects() {
        recentProjects = RecentProject.load()
    }

    /// Delete a recent project from the welcome list and refresh the UI.
    /// Removes the entry from ~/.spire/recent-projects.json; the project
    /// directory itself is left untouched.
    func removeRecentProject(path: String) {
        RecentProject.remove(path: path)
        recentProjects = RecentProject.load()
    }

    private func rootParams(_ root: String?) -> [String: Any] {
        if let root, !root.isEmpty {
            return ["root": root]
        }
        return [:]
    }

    /// Single-round-trip "describe the project" plan: computes the in-memory
    /// structural contract (ScaffoldSpec — NOTHING written to disk) and asks
    /// the LLM for an implementation plan inside it. Returns both so the wizard
    /// can show the plan for OK/Reject. On OK the UI scaffolds (materializes
    /// the spec) then executes the plan.
    /// Calls FFI: createProject/Plan { goal, rootDir, projectName, language, platforms, structure, embedded }
    func planScaffold(goal: String, rootDir: String, projectName: String,
                      language: String = "Rust", platforms: [String] = [],
                      structure: String = "native", embedded: Bool = false) async -> PlanScaffoldResult? {
        do {
            var params: [String: Any] = [
                "goal": goal,
                "rootDir": rootDir,
                "projectName": projectName,
                "language": language,
                "structure": structure
            ]
            if !platforms.isEmpty {
                params["platforms"] = platforms
            }
            if embedded {
                params["embedded"] = true
            }
            let body: [String: Any] = [
                "method": "createProject/Plan",
                "params": params
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            // Surface a key/value `error` field when present.
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any],
               let err = json["error"] as? String {
                Self.logScaffold("createProject/Plan error: \(err)")
                return nil
            }
            return try MessageSerializer.decode(reply)
        } catch {
            Self.logScaffold("createProject/Plan FAILED: \(error.localizedDescription)")
            return nil
        }
    }

    /// Generate a step-by-step plan for creating a new project.
    /// Calls FFI: createProject/GeneratePlan { goal, rootDir, language }
    func generateCreationPlan(goal: String, rootDir: String, language: String = "Rust") async -> PlanGenerationResult? {
        await generateProjectPlan(goal: goal, rootDir: rootDir, language: language)
    }

    /// Generate a plan for a new project from the step-wise wizard's choices.
    /// `platforms` is the selected cross-compilation target registry-id list
    /// (e.g. `["rpi5","rock3c"]`; empty → host/native). `structure` is
    /// `"native" | "single_source" | "hal"`; `embedded` is true for
    /// cross-compile projects (no host target). Calls FFI:
    /// createProject/GeneratePlan { goal, rootDir, language, platforms }.
    func generateProjectPlan(
        goal: String,
        rootDir: String,
        language: String = "Rust",
        platforms: [String] = [],
        structure: String = "native",
        embedded: Bool = false
    ) async -> PlanGenerationResult? {
        do {
            let body: [String: Any] = [
                "method": "createProject/GeneratePlan",
                "params": [
                    "goal": goal,
                    "rootDir": rootDir,
                    "language": language,
                    "platforms": platforms,
                    "structure": structure,
                    "embedded": embedded
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try MessageSerializer.decode(reply) as PlanGenerationResult
        } catch {
            return nil
        }
    }

    /// Execute an entire creation plan. Calls FFI: createProject/ExecutePlan
    func executeCreationPlan(rootDir: String, steps: [CreationStep]) async -> [StepExecutionResult] {
        do {
            let stepsJson = steps.map { step -> [String: Any] in
                var dict: [String: Any] = [
                    "id": step.id,
                    "stepType": step.stepType.rawValue,
                    "description": step.description,
                    "status": step.status.rawValue,
                ]
                if let params = step.parameters {
                    dict["parameters"] = params.value
                }
                if let result = step.result {
                    dict["result"] = result
                }
                return dict
            }
            let body: [String: Any] = [
                "method": "createProject/ExecutePlan",
                "params": [
                    "rootDir": rootDir,
                    "steps": stepsJson
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return (try MessageSerializer.decode(reply) as [StepExecutionResult])
        } catch {
            // silent: print("[executeCreationPlan] error: \(error)")
            return []
        }
    }

    /// Execute a single plan step and return its result.
    func executeStep(_ step: CreationStep, rootDir: String) async -> StepExecutionResult? {
        do {
            var stepDict: [String: Any] = [
                "id": step.id,
                "stepType": step.stepType.rawValue,
                "description": step.description,
                "status": step.status.rawValue,
            ]
            if let params = step.parameters { stepDict["parameters"] = params.value }
            if let result = step.result { stepDict["result"] = result }
            let body: [String: Any] = [
                "method": "createProject/ExecuteStep",
                "params": ["rootDir": rootDir, "step": stepDict]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try MessageSerializer.decode(reply) as StepExecutionResult
        } catch {
            // silent: print("[executeStep] error: \(error)")
            return nil
        }
    }

    /// Send a chat message and get a response.
    /// When a subproject is selected in the project graph, the chat message
    /// is annotated with the modification scope so the LLM knows it may only
    /// modify files within the selected subproject (Level 2 scope).
    /// Create a modification plan via the task-focused plan/create RPC.
    /// - `goal`: description of the modification to plan
    /// - `scopePath`: absolute path when subproject-scoped; nil for project scope
    func createPlan(goal: String, scopePath: String?, language: String? = nil, buildSystem: String? = nil) async -> PlanStatusResult? {
        lastPlanError = nil
        do {
            // The Rust plan/create handler only forwards `goal` to the LLM (it
            // drops extra params). To guarantee the LLM knows the project's
            // stack, embed the language + build system directly in the goal.
            var effectiveGoal = goal
            var context: [String] = []
            if let language, !language.isEmpty {
                // "🦀 Rust" / "🐍 Python" style labels — strip emoji prefixes.
                let clean = language
                    .unicodeScalars
                    .filter { !$0.properties.isEmoji && $0.properties.isEmojiPresentation == false }
                    .map { String($0) }
                    .joined()
                    .trimmingCharacters(in: .whitespaces)
                if !clean.isEmpty { context.append("Language: \(clean)") }
            }
            if let buildSystem, !buildSystem.isEmpty {
                context.append("Build system: \(buildSystem)")
            }
            if !context.isEmpty {
                effectiveGoal = "[Project context: \(context.joined(separator: ", "))]\n\(goal)"
            }

            var params: [String: Any] = ["goal": effectiveGoal]
            if let path = scopePath {
                params["scope"] = "subproject"
                params["scope_path"] = path
            } else {
                params["scope"] = "project"
            }
            // Give the LLM explicit project context so planning uses the right
            // language and build-tool conventions (e.g. Python + pyproject.toml).
            if let language, !language.isEmpty { params["language"] = language }
            if let buildSystem, !buildSystem.isEmpty { params["build_system"] = buildSystem }
            let body: [String: Any] = ["method": "plan/create", "params": params]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            // Surface the backend's key/value `error` field when present
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any],
               let err = json["error"] as? String {
                lastPlanError = err
                return nil
            }
            return try MessageSerializer.decode(reply) as PlanStatusResult
        } catch {
            lastPlanError = error.localizedDescription
            return nil
        }
    }

    /// Approve a generated plan — the Rust core executes its steps sequentially.
    /// Returns true on success, false on failure (details in `lastPlanError`).
    func approvePlan(planId: String) async -> Bool {
        lastPlanError = nil
        do {
            let body: [String: Any] = [
                "method": "plan/approve",
                "params": ["plan_id": planId]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] {
                if let err = json["error"] as? String {
                    lastPlanError = err
                    return false
                }
                return json["ok"] as? Bool ?? false
            }
            return false
        } catch {
            lastPlanError = error.localizedDescription
            return false
        }
    }


    func sendChatMessage(_ text: String) async {
        guard !text.trimmingCharacters(in: .whitespaces).isEmpty else { return }

        let userMsg = ChatMessage(
            id: UUID().uuidString,
            role: .user,
            content: text,
            timestamp: Date()
        )
        messages.append(userMsg)
        isProcessing = true

        // Scope annotation: when a subproject is selected in the project graph,
        // constrain the LLM to modification of that subproject only.
        var effectiveText = text
        if let sub = selectedSubproject {
            effectiveText = "[Scope: subproject \(sub.name) at \(sub.path)]\n\(text)"
        }

        do {
            let command = try MessageSerializer.encode(chat: effectiveText)
            let reply = try await backend.send(command)
            let response = try MessageSerializer.decodeChat(reply)
            messages.append(response)
        } catch {
            let errorMsg = ChatMessage(
                id: UUID().uuidString,
                role: .system,
                content: "Error: \(error.localizedDescription)",
                timestamp: Date()
            )
            messages.append(errorMsg)
        }
        isProcessing = false
    }

    func selectSubproject(_ sub: SubprojectInfo?) {
        selectedSubproject = sub
    }

    /// Close the currently open project and return to the welcome dialog.
    /// Because `SpireBridge.shared` is a singleton, this MUST reset the state
    /// so a fresh "New Window" (Cmd-N) shows WelcomeView instead of the stale
    /// project.
    func closeProject() {
        state = .unconnected
        currentMode = .project
        projectRoot = nil
        selectedSubproject = nil
        showHalContractLint = false
        messages = []
        activePlan = nil
        planVisible = false
        buildEvents = []
        buildRunning = false
    }

    // MARK: - Push Event Stream

    /// A single pushed file-change event from the Rust core.
    struct FileChangeEvent: Decodable {
        let kind: String
        var path: String
        /// True when the changed path is a directory; nil when unknown.
        /// Rust watcher events don't include this — `ProjectInfo.apply`
        /// fills it in from the real filesystem so extension-less files
        /// (Makefile, LICENSE, Dockerfile, …) are never misread as folders.
        var isDirectory: Bool?
    }

    /// A streaming build line pushed from the Rust build module.
    struct BuildEventLine: Decodable, Identifiable, Hashable, Sendable {
        let id: UUID
        let line: String
        let level: String
        let target: String?
        /// File path parsed from "  --> path:line:col" (warning/error blocks).
        let file: String?
        /// Line number parsed from the location line.
        let lineNumber: Int?
        /// The message text for a block-level warning/error.
        let message: String?
        /// Full raw block text (may contain code context for warning/error).
        let detail: String?

        init(id: UUID = UUID(), line: String, level: String, target: String? = nil,
             file: String? = nil, lineNumber: Int? = nil, message: String? = nil,
             detail: String? = nil) {
            self.id = id
            self.line = line
            self.level = level
            self.target = target
            self.file = file
            self.lineNumber = lineNumber
            self.message = message
            self.detail = detail
        }
    }

    /// Live lines from the last build, populated when a build tool completes.
    var buildEvents: [BuildEventLine] = []

    /// Aggregate the current build/lint events into per-file diagnostic badge
    /// counts: [filePath: [severity:count]]. Used by the file tree to show
    /// warning/error badges next to file names.
    var diagnosticBadges: [String: [String: Int]] {
        var result: [String: [String: Int]] = [:]
        for ev in buildEvents {
            guard let file = ev.file else { continue }
            let severity = ev.level == "error" ? "error" : "warning"
            result[file, default: [:]][severity, default: 0] += 1
        }
        return result
    }

    /// True while a hardcoded build tool is running.
    var buildRunning: Bool = false

    /// Add a single build event line (kept for push-event compatibility).
    func appendBuildEvent(line: String, level: String, target: String?) {
        buildEvents.append(BuildEventLine(line: line, level: level, target: target))
    }

    /// Single, long-lived waiter for build events. With tokio::sync::Notify only
    /// ONE waiter is woken per notify_one(), so there must be exactly one consumer
    /// draining the shared buffer — otherwise events get stolen by stale consumers.
    /// The bridge owns this consumer for its entire lifetime.
    private var buildEventDrainTask: Task<Void, Never>?

    /// Start the single build-event consumer. Idempotent — calling again does not
    /// spawn a second waiter (which would steal notifications from the first).
    func startBuildEventConsumer(onEvents: @escaping ([BuildEventLine]) -> Void) {
        guard buildEventDrainTask == nil else { return }
        buildEventDrainTask = Task.detached { [weak self] in
            while !Task.isCancelled {
                guard let json = self?.backend.waitForBuildEvent(timeoutMs: 10000),
                      let data = json.data(using: .utf8),
                      let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]],
                      !arr.isEmpty
                else { continue }
                let lines = arr.compactMap { d -> BuildEventLine? in
                    guard let line = d["line"] as? String else { return nil }
                    return BuildEventLine(
                        line: line,
                        level: d["level"] as? String ?? "info",
                        target: d["target"] as? String,
                        file: d["file"] as? String,
                        lineNumber: d["line_number"] as? Int,
                        message: d["message"] as? String,
                        detail: d["detail"] as? String
                    )
                }
                if !lines.isEmpty {
                    Task { @MainActor in
                        onEvents(lines)
                    }
                }
            }
        }
    }

    /// Stop the single build-event consumer.
    func stopBuildEventConsumer() {
        buildEventDrainTask?.cancel()
        buildEventDrainTask = nil
    }

    /// Parse {"line":...,"level":...,"target":...} JSON objects into BuildEventLines.
    private func parseBuildEvents(_ value: Any?) -> [BuildEventLine] {
        guard let arr = value as? [[String: Any]] else { return [] }
        return arr.compactMap { dict in
            guard let line = dict["line"] as? String else { return nil }
            let level = dict["level"] as? String ?? "info"
            let target = dict["target"] as? String
            let file = dict["file"] as? String
            let lineNumber = dict["line_number"] as? Int
            let message = dict["message"] as? String
            let detail = dict["detail"] as? String
            return BuildEventLine(
                line: line, level: level, target: target,
                file: file, lineNumber: lineNumber, message: message, detail: detail
            )
        }
    }

    /// Push-only stream of file-change events. The FFI call blocks until the
    /// next event arrives (no polling), so the Rust actor drives the UI.
    func eventStream(timeoutMs: UInt32 = 10000) -> AsyncStream<FileChangeEvent> {
        AsyncStream { continuation in
            Task.detached {
                while !Task.isCancelled {
                    guard let json = self.backend.waitForEvent(timeoutMs: timeoutMs) else { continue }
                    guard let data = json.data(using: .utf8),
                          let event = try? JSONDecoder().decode(FileChangeEvent.self, from: data)
                    else { continue }
                    continuation.yield(event)
                }
                continuation.finish()
            }
        }
    }

    // MARK: - MCP Server Commands

    /// Read the last persisted build status (success/duration/timestamp) for
    /// a subproject path from the knowledge graph config.
    func fetchBuildStatus(path: String, target: String? = nil) async -> BuildStatus? {
        do {
            var params: [String: Any] = ["path": path]
            if let target, !target.isEmpty {
                params["target"] = target
            }
            let body: [String: Any] = [
                "method": "project/buildStatus",
                "params": params
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let dict = try JSONSerialization.jsonObject(with: reply) as? [String: Any]
            guard let success = dict?["success"] as? Bool else { return nil }
            // The graph config stores {path, success, duration_secs, timestamp}
            // (RFC3339 from store_build_status). Rust's to_rfc3339() includes
            // nanosecond fractional seconds, e.g. 2026-08-09T08:03:25.123456789Z,
            // which the default ISO8601DateFormatter cannot parse — enable
            // fractional-seconds support with a plain fallback.
            let isoFraction = ISO8601DateFormatter()
            isoFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            let isoPlain = ISO8601DateFormatter()
            let lastBuild = (dict?["timestamp"] as? String).flatMap { ts in
                isoFraction.date(from: ts) ?? isoPlain.date(from: ts)
            }
            let duration = dict?["duration_secs"] as? Double
            return BuildStatus(
                lastBuild: lastBuild,
                success: success,
                output: dict?["output"] as? String,
                errors: [],
                durationSecs: duration
            )
        } catch {
            return nil
        }
    }

    // MARK: - RAG

    /// Fetch all RAG domain summaries (clean id, chunk/source counts, corpus
    /// version) via `rag/list-domains`.
    func fetchRagDomains() async -> [RagDomainInfo] {
        do {
            let body: [String: Any] = ["method": "rag/list-domains", "params": [:]]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try MessageSerializer.decode(reply)
        } catch {
            return []
        }
    }

    /// Install the bundled application-wisdom manifests (spire-actor +
    /// spire-core docs) into the KnowledgeStore scan dirs so RagView lists
    /// them and their Ingest buttons build the corpus.
    func installSpireDocsManifests() async -> Bool {
        let body: [String: Any] = ["method": "rag/install-bundle-manifests", "params": [:]]
        do {
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try? JSONSerialization.jsonObject(with: reply) as? [String: Any],
               json["error"] is String {
                return false
            }
            return true
        } catch {
            return false
        }
    }

    /// Discover the available ingestion manifests (`ingest.yaml`, one per
    /// corpus directory) via `rag/list-manifests`. RAG is project-independent.
    func fetchRagManifests() async -> [RagManifestInfo] {
        do {
            let body: [String: Any] = [
                "method": "rag/list-manifests",
                "params": [:]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try MessageSerializer.decode(reply)
        } catch {
            return []
        }
    }

    /// Fetch persisted per-source ingest status for a domain via
    /// `rag/list-sources` (no ingest required).
    func fetchRagSources(domain: String) async -> [RagSourceStatus] {
        do {
            let body: [String: Any] = [
                "method": "rag/list-sources",
                "params": ["domain": domain]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try? JSONSerialization.jsonObject(with: reply) as? [String: Any],
               json["error"] is String {
                return []
            }
            return try MessageSerializer.decode(reply) as [RagSourceStatus]
        } catch {
            return []
        }
    }

    /// Set the default RAG domain used by `rag/search` when `domain` is omitted
    /// (kept in sync with the corpus selected in the UI).
    func setRagDomain(domain: String) async {
        let body: [String: Any] = [
            "method": "rag/set-domain",
            "params": ["domain": domain]
        ]
        if let data = try? JSONSerialization.data(withJSONObject: body),
           let _ = try? await backend.send(data) {
            // ok
        }
    }

    /// Ingest one canonical `ingest.yaml` via `rag/ingest-graph-config`.
    /// Returns the number of chunks stored (0 when the backend reports an error).
    func ingestRagManifest(path: String) async -> RagIngestReport? {
        do {
            let body: [String: Any] = [
                "method": "rag/ingest-graph-config",
                "params": [
                    "manifest_path": path
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try? JSONSerialization.jsonObject(with: reply) as? [String: Any],
               json["error"] is String {
                return nil
            }
            return try MessageSerializer.decode(reply) as RagIngestReport
        } catch {
            return nil
        }
    }

    /// Semantic search within a RAG domain via `rag/search`.
    func ragSearch(domain: String, query: String, topK: Int = 5) async -> [RagChunkResult] {
        do {
            let body: [String: Any] = [
                "method": "rag/search",
                "params": [
                    "domain": domain,
                    "query": query,
                    "top_k": topK
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try? JSONSerialization.jsonObject(with: reply) as? [String: Any],
               json["error"] is String {
                return []
            }
            return (try? MessageSerializer.decode(reply) as [RagChunkResult]) ?? []
        } catch {
            return []
        }
    }

    /// Fetch diagnostics (errors/warnings/lint findings) for a project or
    /// subproject from the knowledge graph. `path` is the absolute directory
    /// (empty string = whole project).
    func fetchDiagnostics(path: String = "") async -> [DiagnosticEntry] {
        do {
            let body: [String: Any] = [
                "method": "project/diagnostics",
                "params": ["path": path]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try MessageSerializer.decode(reply)
        } catch {
            return []
        }
    }

    /// Fetch the list of MCP servers from the Rust core.
    func fetchMcpServers() async {
        mcpServersLoading = true
        mcpServersError = nil
        defer { mcpServersLoading = false }

        do {
            let command = try MessageSerializer.encode(command: "mcp/servers")
            let reply = try await backend.send(command)
            let servers: [McpServerInfo] = try MessageSerializer.decode(reply)
            mcpServers = servers
        } catch {
            mcpServersError = error.localizedDescription
        }
    }

    /// Fetch all cross-compilation platforms from the Rust core (graph-backed).
    func fetchPlatforms() async -> [Platform] {
        do {
            let body: [String: Any] = ["method": "platforms/list", "params": [:]]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try MessageSerializer.decode(reply)
        } catch {
            return []
        }
    }

    /// Read a file via the in-process filesystem module (filesystem_read) and return its contents.
    func readFile(at path: String) async -> String? {
        do {
            let body: [String: Any] = [
                "method": "tools/call",
                "params": [
                    "tool": "filesystem_read",
                    "args": ["path": path]
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let str = String(data: reply, encoding: .utf8) ?? ""
            // silent: print("[readFile] raw: \(str.prefix(300))")

            // Parse the result. The in-process filesystem module returns:
            //   Ok → {"Ok": "file contents..."}
            //   Err → {"Err": "Failed to read ..."}
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] {
                // Error wrapper
                if let err = json["Err"] as? String {
                    // silent: print("[readFile] filesystem module returned error: \(err)")
                    return nil
                }
                // Success wrapper
                if let text = json["Ok"] as? String {
                    return text
                }
                // Coordinator error
                if let err = json["error"] as? String {
                    // silent: print("[readFile] coordinator error: \(err)")
                    return nil
                }
            }
            // Some modules may return bare strings
            if let str = String(data: reply, encoding: .utf8) {
                return str
            }
            // silent: print("[readFile] could not parse response")
            return nil
        } catch {
            // silent: print("[readFile] error: \(error)")
            return nil
        }
    }

    /// Write a file via the in-process filesystem module (filesystem_write).
    func writeFile(at path: String, content: String) async -> Bool {
        do {
            let body: [String: Any] = [
                "method": "tools/call",
                "params": [
                    "tool": "filesystem_write",
                    "args": ["path": path, "content": content]
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] {
                if json["Err"] is String { return false }
                if json["error"] is String { return false }
                return true
            }
            return false
        } catch {
            return false
        }
    }

    /// Call an in-process build tool over `tools/call` (returns the raw JSON
    /// object). Used by the HAL wizard for the deterministic contract helpers.
    /// Send a raw coordinator method (e.g. "hal/fixPropose") bypassing tools/call.
    private func callRawMethod(_ method: String, args: [String: Any]) async -> [String: Any]? {
        do {
            let body: [String: Any] = ["method": method, "params": args]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try JSONSerialization.jsonObject(with: reply) as? [String: Any]
        } catch {
            return nil
        }
    }

    private func callBuildTool(_ tool: String, args: [String: Any]) async -> [String: Any]? {
        do {
            let body: [String: Any] = [
                "method": "tools/call",
                "params": [
                    "tool": tool,
                    "args": args
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return try JSONSerialization.jsonObject(with: reply) as? [String: Any]
        } catch {
            return nil
        }
    }

    /// Structural C++ syntax check on a header (`cpp_syntax_check`).
    func cppSyntaxCheck(path: String) async -> CppSyntaxReport? {
        guard let json = await callBuildTool("cpp_syntax_check", args: ["path": path]) else { return nil }
        if json["error"] is String { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return nil }
        return try? JSONDecoder().decode(CppSyntaxReport.self, from: data)
    }

    /// HAL state snapshot: contract + per-platform implementation states.
    func halState(root: String) async -> HalStateSnapshot? {
        guard let json = await callBuildTool("hal_state", args: ["root": root]) else { return nil }
        if json["error"] is String { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return nil }
        return try? JSONDecoder().decode(HalStateSnapshot.self, from: data)
    }

    /// HAL fix proposal: LLM-suggested whole-file rewrite (for review).
    func halFixPropose(root: String, path: String) async -> HalFixProposeResult? {
        guard let json = await callRawMethod("hal/fixPropose", args: ["root": root, "path": path]) else { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let result = try? JSONDecoder().decode(HalFixProposeResult.self, from: data) else { return nil }
        return result
    }

    /// Per-file lint + whole-file rewrite prompt for one HAL header.
    func halFixPrompt(root: String, path: String) async -> HalFixPromptResult? {
        guard let json = await callBuildTool("hal_fix_prompt", args: ["root": root, "path": path]) else { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return nil }
        return try? JSONDecoder().decode(HalFixPromptResult.self, from: data)
    }

    /// HAL doc linter: run the documentation-lint tool (`hal_doc_lint`).
    /// Also stores the total issue count on `halContractIssueCount` so the
    /// implementation-action gate (`halContractsValid`) reflects the latest
    /// lint — both the api row badge loader and the right-pane correction
    /// plan call through here.
    func halDocLint(root: String) async -> HalDocLintReport? {
        guard let json = await callBuildTool("hal_doc_lint", args: ["root": root]) else { return nil }
        if json["error"] is String { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return nil }
        let report = try? JSONDecoder().decode(HalDocLintReport.self, from: data)
        if let report {
            let total = report.files.reduce(0) { $0 + $1.issues.count }
            await MainActor.run {
                self.halContractIssueCount = total
            }
        }
        return report
    }

    /// HAL viewer docs: build the human-readable documentation payload (contracts +
    /// datatypes + per-platform status) via `hal_docs`.
    func halDocs(root: String) async -> HalDocReport? {
        guard let json = await callBuildTool("hal_docs", args: ["root": root]) else { return nil }
        if json["error"] is String { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return nil }
        return try? JSONDecoder().decode(HalDocReport.self, from: data)
    }

    /// HAL viewer verify: run the verification tool via `hal_verify`.
    func halVerify(root: String) async -> [HalVerificationIssue] {
        guard let json = await callBuildTool("hal_verify", args: ["root": root]) else { return [] }
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let issues = try? JSONDecoder().decode([HalVerificationIssue].self, from: data) else { return [] }
        return issues
    }

    /// HAL migration (plan): dry-run detect legacy touch points and build a
    /// migration plan (contracts + impl moves, hal/meson.build, subdir('hal')).
    func halMigratePlan(root: String) async -> (plan: HalMigrationPlan?, error: String?) {
        guard let json = await callBuildTool("hal_migrate_plan", args: ["root": root]) else {
            return (nil, "core unavailable")
        }
        if let err = json["error"] as? String {
            return (nil, err)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else {
            return (nil, "parse error")
        }
        if let plan = try? JSONDecoder().decode(HalMigrationPlan.self, from: data) {
            return (plan, nil)
        }
        return (nil, "unexpected plan shape")
    }

    /// HAL sanity check: verify all required components are present on first
    /// open and return a corrective plan for anything missing.
    func halSanityCheck(root: String) async -> (report: HalSanityReport?, error: String?) {
        guard let json = await callBuildTool("hal_sanity_check", args: ["root": root]) else {
            return (nil, "core unavailable")
        }
        if let err = json["error"] as? String {
            return (nil, err)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else {
            return (nil, "parse error")
        }
        if let report = try? JSONDecoder().decode(HalSanityReport.self, from: data) {
            return (report, nil)
        }
        return (nil, "unexpected report shape")
    }

    /// HAL migration (apply): executes a plan returned by `halMigratePlan`.
    func halMigrateApply(root: String, plan: HalMigrationPlan) async -> (result: HalMigrationResult?, error: String?) {
        guard let planData = try? JSONEncoder().encode(plan),
              let planJSON = try? JSONSerialization.jsonObject(with: planData) else {
            return (nil, "plan re-encode failed")
        }
        guard let json = await callBuildTool("hal_migrate_apply", args: ["root": root, "plan": planJSON]) else {
            return (nil, "core unavailable")
        }
        if let err = json["error"] as? String {
            return (nil, err)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else {
            return (nil, "parse error")
        }
        if let result = try? JSONDecoder().decode(HalMigrationResult.self, from: data) {
            return (result, nil)
        }
        return (nil, "unexpected apply shape")
    }

    /// HAL workflow (Stage 0 gate): validate a proposed abstract-class contract
    /// header. Returns `(valid, summary)` or `(false, error)`.
    func halValidateContract(_ header: String) async -> (valid: Bool, summary: String?, error: String?) {
        guard let json = await callBuildTool("hal_validate_contract", args: ["content": header]) else {
            return (false, nil, "core unavailable")
        }
        if let valid = json["valid"] as? Bool, valid {
            return (true, json["summary"] as? String, nil)
        }
        return (false, nil, json["error"] as? String ?? "invalid contract")
    }

    /// HAL workflow (Stage 0 approve): validate-then-persist the approved
    /// contract to `<root>/hal/api/<filename>.hpp`. Same Stage-0 gate as
    /// validate — an invalid header never touches disk.
    func halWriteContract(root: String, filename: String, content: String) async -> (valid: Bool, written: String?, summary: String?, error: String?) {
        guard let json = await callBuildTool("hal_write_contract", args: [
            "root": root, "filename": filename, "content": content
        ]) else {
            return (false, nil, nil, "core unavailable")
        }
        if let valid = json["valid"] as? Bool, valid {
            return (true, json["written"] as? String, json["summary"] as? String, nil)
        }
        return (false, nil, nil, json["error"] as? String ?? "invalid contract")
    }

    /// HAL workflow (Step 3): the missing-implementation coverage, computed
    /// fresh from disk. Returns the full top-level payload: the
    /// `missing` per-interface platform lists (backward-compatible) PLUS the
    /// `platforms` per-platform × interface function-gap map
    /// ({ implemented, missing: [functions], drifted: [signatures] }).
    func halMissingImpls(root: String) async -> ([String: Any], error: String?) {
        guard let json = await callBuildTool("hal_missing_impls", args: ["root": root]) else {
            return ([:], "core unavailable")
        }
        if let err = json["error"] as? String {
            return ([:], err)
        }
        return (json, nil)
    }

    /// HAL workflow (Stage 2): "add target". For a project root + platform,
    /// writes one placeholder per contract interface, wires hal/meson.build,
    /// and re-analyzes so the `missing_implementation` queue surfaces.
    func halAddTarget(root: String, platform: String) async -> (interfaces: [String], placeholders: [String], error: String?) {
        guard let json = await callBuildTool("hal_add_target", args: ["root": root, "platform": platform]) else {
            return ([], [], "core unavailable")
        }
        if let err = json["error"] as? String {
            return ([], [], err)
        }
        let interfaces = (json["interfaces"] as? [String]) ?? []
        let placeholders = (json["placeholders_written"] as? [String]) ?? []
        return (interfaces, placeholders, nil)
    }

    /// Project-level "add platform": scaffold a FULL new platform target into
    /// an existing HAL project — <plat>/ (meson.build + main.cpp) templated from
    /// an existing platform, per-contract `SPIRE-HAL-STUB` placeholders,
    /// hal/meson.build wiring, root `subdir('<plat>')`, meson_options.txt, and
    /// a re-analyze. Returns the scaffolding detail + any errors.
    func halAddPlatform(root: String, platform: String) async -> (interfaces: [String], stubs: [String], needsFill: [String], error: String?) {
        guard let json = await callBuildTool("hal_add_platform", args: ["root": root, "platform": platform]) else {
            return ([], [], [], "core unavailable")
        }
        if let err = json["error"] as? String {
            return ([], [], [], err)
        }
        let interfaces = (json["interfaces"] as? [String]) ?? []
        let stubs = (json["stubs_written"] as? [String]) ?? []
        let needsFill = (json["needs_fill"] as? [String]) ?? []
        return (interfaces, stubs, needsFill, nil)
    }

    /// HAL gap fill — plan (read-only): returns the work items (one per
    /// missing interface on the platform: `none` → scaffold a new class,
    /// `partial` → add only the missing methods).
    func halFillPlan(root: String, platform: String) async -> (items: [[String: Any]], error: String?) {
        guard let json = await callBuildTool("hal_fill_plan", args: ["root": root, "platform": platform]) else {
            return ([], "core unavailable")
        }
        if let err = json["error"] as? String {
            return ([], err)
        }
        let items = (json["plan"] as? [[String: Any]]) ?? []
        return (items, nil)
    }

    /// HAL gap fill — apply: executes an approved plan (writes files, wires
    /// hal/meson.build, re-analyzes).
    func halFillApply(root: String, plan: [[String: Any]]) async -> (written: [String], failures: [String], error: String?) {
        guard let json = await callBuildTool("hal_fill_apply", args: ["root": root, "plan": plan]) else {
            return ([], [], "core unavailable")
        }
        if let err = json["error"] as? String {
            return ([], [], err)
        }
        let written = (json["written"] as? [String]) ?? []
        let failures = (json["failures"] as? [String]) ?? []
        return (written, failures, nil)
    }

    /// SEMANTIC Stage-1 (deterministic half): build the constrained module-pair
    /// implementation prompt for one interface × platform via
    /// `hal_build_impl_prompt` (contract + structured docs + hardware profile +
    /// library hints + clean impl header + meson gate). Read-only — preview
    /// what the LLM will be asked before any generation happens. `libraryHints`
    /// overrides the platform default hints.
    func halBuildImplPrompt(root: String, interface: String, platform: String, libraryHints: String? = nil) async -> HalImplPromptResult? {
        var args: [String: Any] = ["root": root, "interface": interface, "platform": platform]
        if let hints = libraryHints, !hints.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            args["library_hints"] = hints
        }
        guard let json = await callBuildTool("hal_build_impl_prompt", args: args) else { return nil }
        if json["error"] is String { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let result = try? JSONDecoder().decode(HalImplPromptResult.self, from: data) else { return nil }
        return result
    }

    /// SEMANTIC Stage-1 (LLM half): generate a REAL module-pair implementation
    /// via `hal_generate_impl` — deterministic clean declaration header +
    /// LLM-written `.cpp` (fence-stripped + syntax-checked), stale stub
    /// removal, idempotent hal/meson.build wiring and a `meson compile` gate.
    func halGenerateImpl(root: String, interface: String, platform: String, libraryHints: String? = nil) async -> (result: HalGenerateImplResult?, error: String?) {
        var args: [String: Any] = ["root": root, "interface": interface, "platform": platform]
        if let hints = libraryHints, !hints.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            args["library_hints"] = hints
        }
        guard let json = await callBuildTool("hal_generate_impl", args: args) else {
            return (nil, "core unavailable")
        }
        if let err = json["error"] as? String {
            return (nil, err)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let result = try? JSONDecoder().decode(HalGenerateImplResult.self, from: data) else {
            return (nil, "failed to decode generate-impl result")
        }
        return (result, nil)
    }

    /// SEMANTIC Stage-1 (PLAN): preview the proposed module pair (clean header
    /// + LLM .cpp) via `hal_generate_impl_plan` WITHOUT writing anything. The
    /// user approves it, then `halGenerateImplApply` writes it. Returns the
    /// backend error (truncation, contract gate, LLM config…) so the UI can
    /// show exactly what failed.
    func halGenerateImplPlan(root: String, interface: String, platform: String, libraryHints: String? = nil) async -> (plan: HalGenerateImplPlan?, error: String?) {
        var args: [String: Any] = ["root": root, "interface": interface, "platform": platform]
        if let hints = libraryHints, !hints.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            args["library_hints"] = hints
        }
        guard let json = await callBuildTool("hal_generate_impl_plan", args: args) else {
            return (nil, "core unavailable")
        }
        if let err = json["error"] as? String {
            return (nil, err)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let plan = try? JSONDecoder().decode(HalGenerateImplPlan.self, from: data) else {
            return (nil, "failed to decode generate-plan result")
        }
        return (plan, nil)
    }

    /// SEMANTIC Stage-1 (APPLY): write an APPROVED module pair via
    /// `hal_generate_impl_apply` (header + source from the plan), remove stale
    /// stubs, wire hal/meson.build and run the meson compile gate.
    func halGenerateImplApply(root: String, interface: String, platform: String, plan: HalGenerateImplPlan) async -> (result: HalGenerateImplApplyResult?, error: String?) {
        let args: [String: Any] = [
            "root": root,
            "interface": interface,
            "platform": platform,
            "class_name": plan.className,
            "hpp_path": plan.hppPath,
            "cpp_path": plan.cppPath,
            "header": plan.header,
            "source": plan.source,
        ]
        guard let json = await callBuildTool("hal_generate_impl_apply", args: args) else {
            return (nil, "core unavailable")
        }
        if let err = json["error"] as? String {
            return (nil, err)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json),
              let result = try? JSONDecoder().decode(HalGenerateImplApplyResult.self, from: data) else {
            return (nil, "failed to decode apply result")
        }
        return (result, nil)
    }

    /// HAL workflow (Stage 3): diff two approved contract summaries → the
    /// stale-implementation reconcile input (added/removed/changed methods).
    func halDiffContracts(old: String, new: String) async -> (added: [String], removed: [String], changed: [[String]], error: String?) {
        guard let json = await callBuildTool(
            "hal_diff_contracts",
            args: ["old_summary": old, "new_summary": new]
        ) else {
            return ([], [], [], "core unavailable")
        }
        if let err = json["error"] as? String {
            return ([], [], [], err)
        }
        return (
            (json["added"] as? [String]) ?? [],
            (json["removed"] as? [String]) ?? [],
            (json["changed"] as? [[String]]) ?? [],
            nil
        )
    }

    /// Phase 1: scaffold a new project offline via `createProject/Scaffold`.
    /// The Rust core resolves the build module's scaffold_layout, writes all
    /// structural + source-stub files to `root`, runs AnalyzeProject, and
    /// returns the ScaffoldSpec (locked files, fill roots, platforms, layout).
    /// Returns nil on success, error string on failure.
    func scaffoldProject(buildSystem: String, projectName: String, root: String,
                         platforms: [String] = [], structure: String = "native",
                         embedded: Bool = false) async -> String? {
        do {
            let body: [String: Any] = [
                "method": "createProject/Scaffold",
                "params": [
                    "projectName": projectName,
                    "rootDir": root,
                    "language": buildSystem,
                    "platforms": platforms,
                    "structure": structure,
                    "embedded": embedded
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let rawText = String(data: reply, encoding: .utf8) ?? "nil"
            let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any]
            if let err = json?["error"] as? String { return err }
            // Decode the spec and expose it via the state machine. On failure,
            // log the FULL raw reply + the underlying DecodingError so the
            // JSON/Rust mismatch is visible in ~/.spire/logs/spire-scaffold.log.
            do {
                let spec: ScaffoldSpec = try MessageSerializer.decode(reply)
                Self.logScaffold("createProject/Scaffold OK: \(spec.files.count) files, platforms=\(spec.platformTargets)")
                self.scaffoldSpec = spec
                // The observable state must be mutated on the main actor —
                // Observation does not reliably invalidate views for off-main writes.
                await MainActor.run {
                    self.state = .scaffolding(spec: spec)
                }
                return nil
            } catch {
                Self.logScaffold("createProject/Scaffold DECODE FAILED\n  buildSystem=\(buildSystem) platforms=\(platforms) root=\(root)\n  error=\(error)\n  reply=\(rawText)")
                return "Unexpected scaffold response: \(rawText)"
            }
        } catch {
            return error.localizedDescription
        }
    }

    /// Append a line to ~/.spire/logs/spire-scaffold.log (created on demand).
    /// Dedicated file so scaffold decode details survive even when the UI
    /// error surface can't be copy-pasted.
    static func logScaffold(_ line: String) {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".spire").appendingPathComponent("logs")
            .appendingPathComponent("spire-scaffold.log")
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        let stamp = ISO8601DateFormatter().string(from: Date())
        if let handle = try? FileHandle(forWritingTo: url) {
            handle.seekToEndOfFile()
            handle.write(Data("\(stamp) \(line)\n".utf8))
            try? handle.close()
        } else {
            try? Data("\(stamp) \(line)\n".utf8).write(to: url, options: .atomic)
        }
    }

    /// Phase 2: ask the LLM to fill the materialized scaffold inside its fill
    /// roots (`createProject/Fill`). Returns a constrained plan, nil on error.
    func fillProject(goal: String, root: String, spec: ScaffoldSpec) async -> PlanGenerationResult? {
        Self.logScaffold("createProject/Fill CALLED (goal='\(goal)' root=\(root))")
        do {
            // Rust expects snake_case (ScaffoldSpec CodingKeys) and a required
            // `content` on each file — otherwise the guard would deserialize an
            // all-empty spec and lose the structural contract for the fill step.
            let specData = try JSONSerialization.data(withJSONObject: [
                "structural_files": spec.structuralFiles,
                "fill_roots": spec.fillRoots,
                "dependency_sections": spec.dependencySections,
                "platform_targets": spec.platformTargets,
                "build_system": spec.buildSystem,
                "files": spec.files.map {
                    ["path": $0.path, "content": $0.content, "structural": $0.structural]
                }
            ])
            let specDict = try JSONSerialization.jsonObject(with: specData) as? [String: Any] ?? [:]
            let body: [String: Any] = [
                "method": "createProject/Fill",
                "params": [
                    "goal": goal,
                    "rootDir": root,
                    "spec": specDict
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let plan: PlanGenerationResult = try MessageSerializer.decode(reply)
            Self.logScaffold("createProject/Fill OK: \(plan.steps.count) steps")
            // Do NOT transition bridge.state to .filling here. ContentView
            // switches on bridge.state; re-resolving ProjectWizardView mid-fill
            // destroys its local @State fillPlan (spinner stops, plan never
            // renders). The wizard stores the returned plan in its own local
            // @State on the main actor instead.
            return plan
        } catch {
            Self.logScaffold("createProject/Fill FAILED\n  goal=\(goal) root=\(root)\n  error=\(error)")
            return nil
        }
    }

    /// SpireApp requirements pass: derive a VALIDATED AppSpec JSON contract
    /// from the goal (`createProject/GenerateSpec`). The spec is self-healed
    /// against `spec::validate` and persisted to the memory graph. Nothing is
    /// written to disk — it drives deterministic codegen.
    func generateAppSpec(projectName: String, goal: String) async -> (spec: [String: Any]?, error: String?) {
        Self.logScaffold("createProject/GenerateSpec CALLED (project='\(projectName)' goal='\(goal)')")
        let body: [String: Any] = [
            "method": "createProject/GenerateSpec",
            "params": ["projectName": projectName, "goal": goal]
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            guard let object = try JSONSerialization.jsonObject(with: reply) as? [String: Any] else {
                return (nil, "unreadable reply")
            }
            if let err = object["error"] as? String {
                return (nil, err)
            }
            Self.logScaffold("createProject/GenerateSpec OK: \(object["app"] ?? [:])")
            return (object, nil)
        } catch {
            Self.logScaffold("createProject/GenerateSpec FAILED: \(error)")
            return (nil, error.localizedDescription)
        }
    }

    /// Deterministic codegen from a validated AppSpec JSON (`createProject/
    /// GenerateCode`): returns the `write_source_file` skeleton steps
    /// (types/actors/FFI dispatch + Swift wrappers/screens). Execute them with
    /// `executeCreationPlan`.
    func generateCodeSteps(projectName: String, spec: [String: Any]) async -> [CreationStep]? {
        Self.logScaffold("createProject/GenerateCode CALLED (project='\(projectName)')")
        let body: [String: Any] = [
            "method": "createProject/GenerateCode",
            "params": ["projectName": projectName, "spec": spec]
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let steps: [CreationStep] = try MessageSerializer.decode(reply)
            Self.logScaffold("createProject/GenerateCode OK: \(steps.count) skeleton steps")
            return steps
        } catch {
            Self.logScaffold("createProject/GenerateCode FAILED: \(error)")
            return nil
        }
    }

    /// Legacy single-file scaffold helper (superseded by the spec-based
    /// `scaffoldProject` above). Called tools/call build_scaffold and wrote
    /// only build_content + one source stub; the two-phase flow replaces it.
    @available(*, deprecated, message: "Use the spec-based scaffoldProject")
    func scaffoldProjectLegacy(buildSystem: String, projectName: String, root: String, goal: String) async -> String? {
        do {
            let body: [String: Any] = [
                "method": "tools/call",
                "params": [
                    "tool": "build_scaffold",
                    "args": [
                        "project_name": projectName,
                        "build_system": buildSystem,
                        "goal": goal
                    ] as [String: Any]
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any]
            if let err = json?["error"] as? String { return err }

            // Two possible response envelopes:
            // 1. MCP-style: {"result": {"content":[{"type":"text","text":"{json}"}]}}
            // 2. Direct:    {<ScaffoldOutput fields>...} or {"Ok": {<fields>...}}
            var payload: [String: Any]?
            if let result = json?["result"] as? [String: Any],
               let content = result["content"] as? [[String: Any]],
               let first = content.first,
               let text = first["text"] as? String {
                payload = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any]
            }
            if payload == nil {
                payload = json
            }
            guard var output = payload else {
                return "Unexpected scaffold response format: \(String(data: reply, encoding: .utf8) ?? "nil")"
            }
            if let err = output["error"] as? String { return err }
            if let ok = output["Ok"] as? [String: Any] { output = ok }

            guard let buildFile = output["build_file"] as? String,
                  let buildContent = output["build_content"] as? String,
                  let sourceFile = output["source_file"] as? String,
                  let sourceContent = output["source_content"] as? String else {
                return "Scaffold response missing file fields: \(String(data: reply, encoding: .utf8)?.prefix(300) ?? "")"
            }
            let sourceDir = output["source_dir"] as? String ?? ""

            // Write the two files, creating parent dirs implicitly via filesystem_write.
            let cleanRoot = root.hasSuffix("/") ? String(root.dropLast()) : root
            let buildPath = "\(cleanRoot)/\(buildFile)"
            let sourcePath = sourceDir.isEmpty ? "\(cleanRoot)/\(sourceFile)" : "\(cleanRoot)/\(sourceDir)/\((sourceFile as NSString).lastPathComponent)"

            if !(await writeFile(at: buildPath, content: buildContent)) {
                return "Failed to write build config at \(buildPath)"
            }
            if !(await writeFile(at: sourcePath, content: sourceContent)) {
                return "Failed to write source file at \(sourcePath)"
            }
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    /// Result of running a hardcoded build tool (build, test, clean, lint, format).
    struct BuildToolResult {
        let success: Bool
        let output: String
        let command: String?
        let durationSecs: Double?
        let error: String?
        /// Streaming build lines: [{line, level, target}]
        let buildEvents: [BuildEventLine]
    }

    /// Run a hardcoded build tool against a subproject directory.
    /// `tool` is one of: build_build, build_test, build_clean, build_lint, build_format.
    /// `path` is the ABSOLUTE path to the subproject directory.
    func runBuildTool(tool: String, path: String, language: String = "Rust", package: String? = nil) async -> BuildToolResult? {
        do {
            var body: [String: Any] = [
                "method": "tools/call",
                "params": [
                    "tool": tool,
                    "args": [
                        "path": path,
                        "language": language
                    ] as [String: Any]
                ]
            ]
            // Pass the workspace package name when targeting a specific subproject
            // member (e.g. `cargo build --package spire-code`).
            if let package {
                if var params = body["params"] as? [String: Any],
                   var args = params["args"] as? [String: Any] {
                    args["package"] = package
                    params["args"] = args
                    body["params"] = params
                }
            }
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any]
            if let err = json?["error"] as? String {
                // silent: print("[runBuildTool] error: \(err)")
                return BuildToolResult(success: false, output: "", command: nil, durationSecs: nil, error: err, buildEvents: [])
            }
            // The module returns {"result": {"content":[{"type":"text","text": "{...json...}"}], "isError":false}}
            // where the inner text is JSON: {"success":true,"output":"...","command":"..."}
            if let result = json?["result"] as? [String: Any],
               let content = result["content"] as? [[String: Any]],
               let first = content.first,
               let text = first["text"] as? String,
               let payload = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] {
                return BuildToolResult(
                    success: payload["success"] as? Bool ?? false,
                    output: payload["output"] as? String ?? text,
                    command: payload["command"] as? String,
                    durationSecs: payload["durationSecs"] as? Double,
                    error: payload["error"] as? String,
                    buildEvents: parseBuildEvents(payload["buildEvents"])
                )
            }
            // Maybe the reply itself has success/output
            if let success = json?["success"] as? Bool {
                return BuildToolResult(
                    success: success,
                    output: json?["output"] as? String ?? String(data: reply, encoding: .utf8) ?? "",
                    command: json?["command"] as? String,
                    durationSecs: json?["durationSecs"] as? Double,
                    error: json?["error"] as? String,
                    buildEvents: parseBuildEvents(json?["buildEvents"])
                )
            }
            return nil
        } catch {
            // silent: print("[runBuildTool] error: \(error)")
            return nil
        }
    }

    /// Fetch Markdown documentation for a dependency package via the
    /// language module (crates.io for Rust, npm registry, PyPI, etc.).
    func fetchDependencyDocs(name: String, version: String?, language: String = "Rust") async -> String? {
        do {
            var params: [String: Any] = [
                "tool": "build_dependency_docs",
                "args": [
                    "name": name,
                    "language": language
                ]
            ]
            if let version, !version.isEmpty {
                var args = params["args"] as? [String: Any] ?? [:]
                args["version"] = version
                params["args"] = args
            }
            let body: [String: Any] = [
                "method": "tools/call",
                "params": params
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let str = String(data: reply, encoding: .utf8) ?? ""
            // silent: print("[fetchDependencyDocs] raw: \(str.prefix(300))")

            // The module returns {"result": {"content":[{"type":"text","text":"{...json...}"}], "isError": false}}
            // where the inner text is itself JSON containing "markdown".
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] {
                if let err = json["error"] as? String { return nil }
                if let result = json["result"] as? [String: Any] {
                    if let content = result["content"] as? [[String: Any]],
                       let first = content.first,
                       let text = first["text"] as? String {
                        // The text is JSON: {"markdown": "..."}
                        if let md = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
                           let markdown = md["markdown"] as? String {
                            return markdown
                        }
                        // Fallback: maybe the text IS the markdown
                        return text
                    }
                    if let isError = result["isError"] as? Bool, isError { return nil }
                }
            }
            return nil
        } catch {
            // silent: print("[fetchDependencyDocs] error: \(error)")
            return nil
        }
    }

    // MARK: - LLM Settings

    /// Load LLM settings from the Rust core via `config/getAll`.
    func fetchLlmConfig() async {
        llmConfigLoading = true
        defer { llmConfigLoading = false }
        do {
            let command = try MessageSerializer.encode(command: "config/getAll")
            let reply = try await backend.send(command)
            let dict = (try JSONSerialization.jsonObject(with: reply) as? [String: Any])?["config"] as? [String: Any] ?? [:]
            llmConfig.apiKey = dict["deepseek.api_key"] as? String ?? ""
            llmConfig.planningModel = dict["deepseek.planning_model"] as? String ?? "deepseek-v4-pro"
            llmConfig.codingModel = dict["deepseek.coding_model"] as? String ?? "deepseek-v4-pro"
            llmConfig.tavilyApiKey = dict["tavily.api_key"] as? String ?? ""
        } catch {
            // silent: print("[fetchLlmConfig] error: \(error)")
        }
    }

    /// Save one config key via `config/set`.
    func saveLlmConfigKey(_ key: String, _ value: String) async -> Bool {
        do {
            let body: [String: Any] = [
                "method": "config/set",
                "params": ["key": key, "value": value]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            return (try JSONSerialization.jsonObject(with: reply) as? [String: Any])?["success"] as? Bool ?? false
        } catch {
            // silent: print("[saveLlmConfigKey] error: \(error)")
            return false
        }
    }

    /// Save all LLM settings (DeepSeek + Tavily web-search key).
    func saveLlmConfig(_ config: LLMConfig) async -> Bool {
        let keyOk = await saveLlmConfigKey("deepseek.api_key", config.apiKey)
        let planningOk = await saveLlmConfigKey("deepseek.planning_model", config.planningModel)
        let codingOk = await saveLlmConfigKey("deepseek.coding_model", config.codingModel)
        let tavilyOk = await saveLlmConfigKey("tavily.api_key", config.tavilyApiKey)
        return keyOk && planningOk && codingOk && tavilyOk
    }

    /// Fetch every available tool from all backends (core modules + build
    /// modules + internal + MCP servers) via `tools/list`.
    func fetchAllTools() async {
        allToolsLoading = true
        defer { allToolsLoading = false }

        do {
            let command = try MessageSerializer.encode(command: "tools/list")
            let reply = try await backend.send(command)
            let tools: [McpToolInfo] = try MessageSerializer.decode(reply)
            allTools = tools
        } catch {
            allTools = []
        }
    }

    /// Fetch tools for a specific MCP server.
    func fetchTools(for serverName: String) async {
        guard !mcpToolsLoading.contains(serverName) else { return }
        mcpToolsLoading.insert(serverName)
        defer { mcpToolsLoading.remove(serverName) }

        do {
            // Build the JSON command manually for the nested method/params structure
            let body: [String: Any] = [
                "method": "mcp/getTools",
                "params": ["serverName": serverName]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            let tools: [McpToolInfo] = try MessageSerializer.decode(reply)
            mcpTools[serverName] = tools
        } catch {
            mcpTools[serverName] = []
        }
    }

    // MARK: - Spec design RPC (free-form AppSpec design step)

    /// One spec-design RPC round trip: returns the parsed reply object (or nil)
    /// plus a non-nil error string on failure.
    private func specDesignCall(method: String, params: [String: Any]) async -> (value: Any?, error: String?) {
        let body: [String: Any] = ["method": method, "params": params]
        do {
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            guard let object = try JSONSerialization.jsonObject(with: reply) as? [String: Any] else {
                return (nil, "spec-design: unreadable reply")
            }
            if let err = object["error"] as? String {
                return (nil, err)
            }
            return (object, nil)
        } catch {
            return (nil, error.localizedDescription)
        }
    }

    /// Open a design session for the project. Returns the initial state.
    func specDesignStart(projectName: String, goal: String, reset: Bool = false) async -> (state: SpecDesignState?, error: String?) {
        let (value, error) = await specDesignCall(method: "spec-design/start", params: ["projectName": projectName, "goal": goal, "reset": reset])
        guard error == nil else { return (nil, error) }
        return (SpecDesignState(json: value as Any), nil)
    }

    /// Ask the LLM a free-form brainstorm question INSIDE the design session
    /// (the design transcript owns the conversation). Appends user + assistant
    /// turns server-side; returns the answer text and the updated state.
    func specDesignAsk(projectName: String, text: String, docs: Bool = false, web: Bool = false) async -> (text: String?, state: SpecDesignState?, error: String?) {
        let (value, error) = await specDesignCall(method: "spec-design/ask", params: ["projectName": projectName, "text": text, "docs": docs, "web": web])
        guard error == nil else { return (nil, nil, error) }
        guard let dict = value as? [String: Any] else {
            return (nil, nil, "spec-design/ask: unreadable reply")
        }
        return (dict["text"] as? String, SpecDesignState(json: dict["state"]), nil)
    }

    /// Mirror a user turn into the design transcript (kept for external
    /// turn-mirroring; the design view normally uses `specDesignAsk`).
    func specDesignReply(projectName: String, text: String) async -> (state: SpecDesignState?, error: String?) {
        let (value, error) = await specDesignCall(method: "spec-design/reply", params: ["projectName": projectName, "text": text])
        guard error == nil else { return (nil, error) }
        return (SpecDesignState(json: value as Any), nil)
    }

    /// Mirror any other turn (e.g. the assistant's brainstorm reply) into the
    /// design transcript.
    func specDesignTurn(projectName: String, role: String, text: String) async -> (state: SpecDesignState?, error: String?) {
        let (value, error) = await specDesignCall(method: "spec-design/turn", params: ["projectName": projectName, "role": role, "text": text])
        guard error == nil else { return (nil, error) }
        return (SpecDesignState(json: value as Any), nil)
    }

    /// Return to free-form editing (a decided spec may not meet requirements).
    func specDesignReopen(projectName: String) async -> (state: SpecDesignState?, error: String?) {
        let (value, error) = await specDesignCall(method: "spec-design/reopen", params: ["projectName": projectName])
        guard error == nil else { return (nil, error) }
        return (SpecDesignState(json: value as Any), nil)
    }

    /// Current session state (mode + turn count).
    func specDesignState(projectName: String) async -> SpecDesignState? {
        let (value, error) = await specDesignCall(method: "spec-design/state", params: ["projectName": projectName])
        guard error == nil else { return nil }
        return SpecDesignState(json: value as Any)
    }

    // MARK: - AppSpec design (post-open, project graph backed)

    /// The project name the current design session targets (leaf of the
    /// project root, matching the Rust session key).
    var designProjectName: String {
        guard let root = projectRoot else { return "" }
        return (root as NSString).lastPathComponent
    }

    /// After Decide, materialize the derived AppSpec skeleton into the OPEN
    /// project and refresh the analysis.
    func runSpecDesignCodegen(spec: [String: Any]) async {
        let name = designProjectName
        guard !name.isEmpty else { return }
        Self.logScaffold("Design AppSpec decided for '\(name)' — running codegen")
        if let steps = await generateCodeSteps(projectName: name, spec: spec), !steps.isEmpty,
           let root = projectRoot {
            _ = await executeCreationPlan(rootDir: root, steps: steps)
            await openProject(root: root)
        }
    }

}
