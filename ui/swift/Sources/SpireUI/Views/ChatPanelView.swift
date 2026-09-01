import SwiftUI

/// Slide-over secondary chat panel. Accessed via the 💬 toolbar button.
struct ChatPanelView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var inputText: String = ""
    @FocusState private var inputFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Label("Chat", systemImage: "message.fill")
                    .font(.headline)
                Spacer()
                Button {
                    bridge.showChat = false
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                        .font(.title3)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(theme.surface)

            Divider()
                .overlay(theme.divider)

            // Messages
            ScrollViewReader { proxy in
                ScrollView(.vertical, showsIndicators: true) {
                    if bridge.messages.isEmpty {
                        emptyChat
                    } else {
                        LazyVStack(spacing: 8) {
                            ForEach(bridge.messages) { msg in
                                MessageBubble(message: msg)
                                    .id(msg.id)
                            }
                        }
                        .padding(12)
                    }
                }
                .onChange(of: bridge.messages.count) { _, _ in
                    if let last = bridge.messages.last {
                        withAnimation {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                }
            }

            Divider()
                .overlay(theme.divider)

            // Input bar
            HStack(spacing: 8) {
                TextField("Ask about the project...", text: $inputText)
                    .textFieldStyle(.plain)
                    .focused($inputFocused)
                    .onSubmit { sendMessage() }
                    .disabled(bridge.isProcessing)

                if bridge.isProcessing {
                    ProgressView()
                        .controlSize(.small)
                }

                Button(action: sendMessage) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .buttonStyle(.plain)
                .disabled(inputText.trimmingCharacters(in: .whitespaces).isEmpty || bridge.isProcessing)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(theme.surface)
        }
        .frame(width: 360)
        .background(theme.background)
    }

    private var emptyChat: some View {
        VStack(spacing: 12) {
            Spacer()
            Image(systemName: "message.fill")
                .font(.system(size: 36))
                .foregroundStyle(.tertiary)
            Text("Ask questions about\nyour project structure")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    private func sendMessage() {
        let text = inputText
        inputText = ""
        Task {
            await bridge.sendChatMessage(text)
        }
    }
}

/// A single chat message bubble.
struct MessageBubble: View {
    @Environment(AppTheme.self) private var theme
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.role == .user {
                Spacer()
                bubbleContent
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(theme.accent)
                    )
            } else {
                bubbleContent
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .fill(theme.chatSpeakerBackground)
                    )
                Spacer()
            }
        }
    }

    private var bubbleContent: some View {
        Text(message.content)
            .font(.callout)
            .foregroundStyle(message.role == .user ? .white : .primary)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
    }
}