import SwiftUI

/// Displays the generated creation plan with per-step status and an execute button.
struct PlanView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var executedSteps: [String: StepExecutionResult] = [:]
    @State private var errorMessage: String?

    var body: some View {
        VStack(spacing: 0) {
            if let plan = bridge.creationPlan {
                // Plan header
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Image(systemName: "map.fill")
                            .foregroundStyle(.orange)
                        Text("Creation Plan")
                            .font(.headline)
                        Spacer()
                        BuildTypeBadge(buildType: plan.language)
                    }
                    Text("Root: \(plan.rootDir)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Divider()
                }
                .padding(.horizontal)
                .padding(.vertical, 8)
                .background(theme.surface)

                // Step list
                List {
                    ForEach(plan.steps) { step in
                        StepRow(step: step, result: executedSteps[step.id])
                    }
                }
                .listStyle(.inset)

                // Footer
                HStack {
                    if let err = errorMessage {
                        Text(err)
                            .font(.caption)
                            .foregroundStyle(.red)
                            .lineLimit(2)
                        Spacer()
                    }
                    Spacer()
                    Button {
                        executePlan()
                    } label: {
                        if bridge.creationExecuting {
                            ProgressView().scaleEffect(0.8)
                            Text("Executing…")
                        } else {
                            Label("Execute Plan", systemImage: "play.fill")
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(bridge.creationExecuting)
                }
                .padding()
            } else {
                ContentUnavailableView(
                    "No Plan",
                    systemImage: "map",
                    description: Text("Generate a plan first from the new project form.")
                )
            }
        }
    }

    private func executePlan() {
        guard let plan = bridge.creationPlan else { return }
        if case .creating(let plan, _) = bridge.state {
            bridge.state = .creating(plan: plan, executing: true)
        }
        errorMessage = nil
        executedSteps = [:]

        Task {
            for step in plan.steps {
                if let result = await bridge.executeStep(step, rootDir: plan.rootDir) {
                    await MainActor.run {
                        executedSteps[result.stepId] = result
                    }
                } else {
                    await MainActor.run {
                        executedSteps[step.id] = StepExecutionResult(
                            stepId: step.id,
                            success: false,
                            message: "Step execution failed"
                        )
                    }
                }
            }
            await MainActor.run {
                bridge.currentMode = .project
            }
            // After execution, re-analyze the freshly scaffolded project
            // and transition to the idle (project dashboard) state.
            await bridge.fetchProjectAnalysis(projectRoot: plan.rootDir)
        }
    }
}

/// A single row showing step icon, name, status, and result message.
private struct StepRow: View {
    let step: CreationStep
    let result: StepExecutionResult?

    private var statusColor: Color {
        if let result {
            return result.success ? .green : .red
        }
        switch step.status {
        case .completed: return .green
        case .failed: return .red
        case .executing: return .orange
        case .pending: return .secondary
        }
    }

    private var statusIcon: String {
        if let result {
            return result.success ? "checkmark.circle.fill" : "xmark.circle.fill"
        }
        switch step.status {
        case .completed: return "checkmark.circle.fill"
        case .failed: return "xmark.circle.fill"
        case .executing: return "arrow.triangle.2.circlepath"
        case .pending: return "circle"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Image(systemName: step.stepType.systemImage)
                    .foregroundStyle(.secondary)
                    .frame(width: 22)
                Text(step.description)
                    .font(.body)
                Spacer()
                Image(systemName: statusIcon)
                    .foregroundStyle(statusColor)
            }
            if let result {
                Text(result.message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .padding(.vertical, 2)
    }
}