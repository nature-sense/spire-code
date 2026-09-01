import SwiftUI

/// Task-focused modification planning sheet. Bypasses the chat panel —
/// the user enters a goal, the LLM generates a plan, and the user approves/rejects.
struct PlanSheetView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo
    let selectedSubproject: SubprojectInfo?

    @State private var goalText: String = ""
    @State private var plan: PlanStatusResult?
    @State private var isGenerating = false
    @State private var isExecuting = false
    @State private var errorMessage: String?

    private var scopeTitle: String {
        if let sub = selectedSubproject { return "\(sub.name) (\(sub.buildSystem))" }
        return project.name
    }
    private var scopePath: String? { selectedSubproject?.path }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            scopeBadge
            goalEditor
            if let err = errorMessage {
                Text(err).font(.caption).foregroundStyle(.red)
            }
            generateButton
            if let p = plan {
                Divider()
                Text("Plan: \(p.goal)").font(.subheadline.weight(.semibold))
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(p.steps.enumerated()), id: \.element.id) { _, step in
                            HStack(spacing: 6) {
                                Image(systemName: step.status == "completed" ? "checkmark.circle.fill" : "circle")
                                    .foregroundStyle(step.status == "completed" ? Color.green : Color.secondary)
                                Text("\(step.order). \(step.description)").font(.callout)
                            }
                        }
                    }.padding(.vertical, 4)
                }
                .frame(maxHeight: 220)
                HStack {
                    Button("Reject", role: .destructive) {
                        plan = nil
                        goalText = ""
                        errorMessage = nil
                    }
                    Spacer()
                    Button("Approve & Execute") {
                        guard let p = plan else { return }
                        isExecuting = true
                        errorMessage = nil
                        Task {
                            let ok = await bridge.approvePlan(planId: p.planId)
                            isExecuting = false
                            if ok {
                                plan = nil
                                goalText = ""
                            } else {
                                errorMessage = bridge.lastPlanError ?? "Failed to execute plan"
                            }
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(isExecuting)
                    if isExecuting {
                        ProgressView().controlSize(.small)
                    }
                }
            }
            Spacer()
        }
        .padding(8)
    }

    private var scopeBadge: some View {
        Text(scopeTitle)
            .font(.caption.weight(.semibold))
            .foregroundColor(.white)
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(theme.accent).cornerRadius(4)
            .help(scopePath ?? "Project scope")
    }

    private var goalEditor: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Goal")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            TextEditor(text: $goalText)
                .frame(height: 80)
                .font(.body)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(theme.border, lineWidth: 1)
                )
        }
    }

    private var generateButton: some View {
        Button {
            isGenerating = true
            errorMessage = nil
            Task {
                // Pass the project's language + build system so the LLM knows the
                // actual stack (e.g. Python + pyproject.toml), not just the goal.
                var language: String?
                var buildSystem: String?
                if let sub = selectedSubproject {
                    language = sub.language
                    buildSystem = sub.buildSystem
                } else if let primary = project.languages.max(by: { $0.value < $1.value }) {
                    language = primary.key
                }
                let result = await bridge.createPlan(
                    goal: goalText,
                    scopePath: scopePath,
                    language: language,
                    buildSystem: buildSystem
                )
                isGenerating = false
                if let r = result {
                    plan = r
                    bridge.activePlan = r   // expose to the right-pane task list
                    bridge.planVisible = true
                } else {
                        errorMessage = bridge.lastPlanError ?? "Failed to generate plan"
                }
            }
        } label: {
            if isGenerating {
                ProgressView().controlSize(.small)
                Text("Generating…")
            } else {
                Label("Generate Plan", systemImage: "sparkles")
            }
        }
        .buttonStyle(.borderedProminent)
        .disabled(goalText.trimmingCharacters(in: .whitespaces).isEmpty || isGenerating)
    }
}