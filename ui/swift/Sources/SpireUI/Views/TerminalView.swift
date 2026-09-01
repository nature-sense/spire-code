import SwiftUI
import AppKit
import SwiftTerm

/// An embedded, fully interactive terminal backed by SwiftTerm's real
/// VT100/xterm emulator. It correctly decodes the stateful 2-D byte stream
/// interactive shells emit (`\r`, `ESC[K`/`ESC[J` erase, cursor moves,
/// bracketed paste) — the way my earlier hand-rolled text renderer could not.
struct TerminalView: NSViewRepresentable {
    /// Working directory for the shell (the open project's root).
    let cwd: String?

    func makeNSView(context: Context) -> LocalProcessTerminalView {
        let term = LocalProcessTerminalView(frame: .zero)
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        // SwiftTerm starts the process with the host cwd; start zsh with an
        // explicit `cd` so the shell lands in the project directory.
        let cmd: String
        if let dir = cwd, !dir.isEmpty {
            let escaped = dir.replacingOccurrences(of: "'", with: "'\\''")
            cmd = "cd '\(escaped)' 2>/dev/null || true; exec \(shell) -l"
        } else {
            cmd = "exec \(shell) -l"
        }
        term.startProcess(executable: "/bin/zsh", args: ["-c", cmd])
        return term
    }

    func updateNSView(_ nsView: LocalProcessTerminalView, context: Context) {
        // Static configuration — nothing to update.
    }
}

/// Opens an embedded terminal in a floating window (same pattern as
/// `PlatformPortal`/`RagPortal`). `cwd` defaults to the open project's root.
enum TerminalPortal {
    private static var windows: [NSWindow] = []

    @MainActor
    static func open(cwd: String?) {
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 600),
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered, defer: false
        )
        w.title = "Terminal"
        w.isReleasedWhenClosed = false
        w.contentViewController = NSHostingController(rootView: TerminalView(cwd: cwd))
        w.setContentSize(NSSize(width: 900, height: 600))
        if let m = NSApp.mainWindow?.frame {
            w.setFrameOrigin(NSPoint(x: m.midX - 450, y: m.midY - 300))
        } else {
            w.center()
        }
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: w, queue: .main
        ) { _ in
            windows.removeAll { $0 === w }
        }
    }
}