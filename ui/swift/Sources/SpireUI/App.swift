import SwiftUI
import AppKit

// MARK: - Menu command notifications
//
// SwiftUI `.commands` closures execute in the Scene/App context, where
// `@Observable` changes do NOT reliably invalidate views that read the same
// object (the file picker opens, but mutations to SpireBridge.shared don't
// re-render ContentView). The reliable pattern: commands post notifications,
// and ContentView — running INSIDE the SwiftUI observation cycle — handles
// them by calling methods on its `@Environment(SpireBridge.self)` bridge.

enum MenuCommand {
    static let newProject = Notification.Name("SpireApp.menu.newProject")
    static let openProject = Notification.Name("SpireApp.menu.openProject")
    static let refreshProject = Notification.Name("SpireApp.menu.refreshProject")
    static let toggleChat = Notification.Name("SpireApp.menu.toggleChat")
    static let showSettings = Notification.Name("SpireApp.menu.showSettings")
    static let designSpec = Notification.Name("SpireApp.menu.designSpec")
}

@main
struct SpireApp: App {
    @State private var bridge = SpireBridge.shared
    @State private var theme = AppTheme()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(bridge)
                .environment(theme)
                .environment(\.sizeCategory, theme.textScale)
                .tint(theme.accent)
                .frame(minWidth: 1000, minHeight: 700)
                .onAppear {
                    // NSApp cannot be used at App struct init() time (it's nil
                    // before the application singleton is created). Activate here
                    // after the window appears, so text fields + open panels
                    // accept keyboard input even when launched from a terminal.
                    DispatchQueue.main.async {
                        NSApp.setActivationPolicy(.regular)
                        NSApp.activate(ignoringOtherApps: true)
                        theme.applyWindowAppearance()
                    }
                }
                .onChange(of: theme.effectiveTier) { _, _ in
                    theme.applyWindowAppearance()
                }
                .preferredColorScheme(theme.colorScheme)
        }
        .windowStyle(.titleBar)
        .windowToolbarStyle(.unifiedCompact)
        .windowResizability(.contentSize)
        .commands {
            // ── App menu ──
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") {
                    NotificationCenter.default.post(name: MenuCommand.showSettings, object: nil)
                }
                .keyboardShortcut(",", modifiers: .command)
            }

            // The system "New Window" (Cmd-N) is kept by default so WindowGroup
            // can recreate a closed window. The WelcomeView is the single entry
            // point for opening/creating projects.

            // ── View menu ──
            CommandGroup(after: .toolbar) {
                Button("Refresh Project") {
                    NotificationCenter.default.post(name: MenuCommand.refreshProject, object: nil)
                }
                .keyboardShortcut("r", modifiers: .command)

                Button("Toggle Chat") {
                    NotificationCenter.default.post(name: MenuCommand.toggleChat, object: nil)
                }
                .keyboardShortcut("c", modifiers: [.command, .shift])

                Button("Design AppSpec…") {
                    NotificationCenter.default.post(name: MenuCommand.designSpec, object: nil)
                }
                .keyboardShortcut("d", modifiers: [.command, .shift])
                .help("Open the free-form AppSpec design session for the current project")

                Divider()

                // ── Theme submenu ──
                Picker("Appearance", selection: Binding(
                    get: { theme.tier },
                    set: { theme.tier = $0 }
                )) {
                    ForEach(AppTheme.Tier.allCases) { tier in
                        Label(tier.displayName, systemImage: tier.systemImage)
                            .tag(tier)
                    }
                }
            }
        }
    }
}