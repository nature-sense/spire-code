import SwiftUI
import AppKit
import Observation

/// App-wide theme with three tiers: System, Very Dark, and High Contrast.
///
/// The "Very Dark" tier forces pure-dark surfaces; the "High Contrast" tier
/// additionally uses black backgrounds with bright gold accents and thick,
/// visible borders. When the user enables "Increase Contrast" in System
/// Settings → Accessibility, the effective tier automatically becomes
/// .highContrast regardless of the manually selected tier.
@Observable
final class AppTheme {
    enum Tier: String, Codable, CaseIterable, Identifiable {
        case system
        case dark
        case highContrast

        var id: String { rawValue }

        var displayName: String {
            switch self {
            case .system: return "System"
            case .dark: return "Very Dark"
            case .highContrast: return "High Contrast"
            }
        }

        var systemImage: String {
            switch self {
            case .system: return "circle.lefthalf.filled"
            case .dark: return "moon.fill"
            case .highContrast: return "sun.max.fill"
            }
        }
    }

    /// User-picked tier. Persisted in UserDefaults. Accessibility "Increase
    /// Contrast" may override this at render time (see `effectiveTier`).
    var tier: Tier {
        didSet {
            guard tier != oldValue else { return }
            UserDefaults.standard.set(tier.rawValue, forKey: Self.prefsKey)
            applyWindowAppearance()
        }
    }

    private static let prefsKey = "spire.appTheme.tier"

    init() {
        if let raw = UserDefaults.standard.string(forKey: Self.prefsKey),
           let saved = Tier(rawValue: raw) {
            tier = saved
        } else {
            tier = .system
        }
    }

    // MARK: - Effective tier

    /// The tier actually used for rendering. If the user has "Increase
    /// Contrast" enabled in System Settings → Accessibility, we always
    /// render high-contrast.
    var effectiveTier: Tier {
        if NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast {
            return .highContrast
        }
        return tier
    }

    /// The SwiftUI color scheme to force, or nil to follow the system.
    var colorScheme: ColorScheme? {
        switch effectiveTier {
        case .system: return nil
        case .dark, .highContrast: return .dark
        }
    }

    /// Global text scale: bump Dynamic Type so all semantic text
    /// (caption/body/headline/etc.) grows ~20% app-wide. Two steps
    /// (.extraExtraLarge) raises body ~23% while macOS's dampened caption
    /// styles grow ~18% — the closest uniform match to +20%.
    var textScale: ContentSizeCategory { .extraExtraLarge }

    // MARK: - Semantic colors

    /// Main window background.
    var background: Color {
        switch effectiveTier {
        case .system: return Color(NSColor.windowBackgroundColor)
        case .dark: return Color(red: 0.09, green: 0.09, blue: 0.10)
        case .highContrast: return .black
        }
    }

    /// Secondary / panel / card background.
    var surface: Color {
        switch effectiveTier {
        case .system: return Color(NSColor.controlBackgroundColor)
        case .dark: return Color(red: 0.15, green: 0.15, blue: 0.16)
        case .highContrast: return Color(red: 0.07, green: 0.07, blue: 0.07)
        }
    }

    /// Text / code viewer background.
    var textBackground: Color {
        switch effectiveTier {
        case .system: return Color(NSColor.textBackgroundColor)
        case .dark: return Color(red: 0.09, green: 0.09, blue: 0.10)
        case .highContrast: return .black
        }
    }

    /// Divider / separator color.
    var divider: Color {
        switch effectiveTier {
        case .system: return Color(.separatorColor)
        case .dark: return Color(red: 0.25, green: 0.25, blue: 0.27)
        case .highContrast: return Color(red: 0.55, green: 0.55, blue: 0.55)
        }
    }

    /// Generic border / stroke color.
    var border: Color {
        switch effectiveTier {
        case .system: return Color.gray.opacity(0.25)
        case .dark: return Color(red: 0.32, green: 0.32, blue: 0.34)
        case .highContrast: return Color.white.opacity(0.85)
        }
    }

    /// Button / chip fill background.
    var buttonBackground: Color {
        switch effectiveTier {
        case .system: return Color.gray.opacity(0.15)
        case .dark: return Color(red: 0.20, green: 0.20, blue: 0.22)
        case .highContrast: return Color.white.opacity(0.18)
        }
    }

    /// Selected tab / row highlight background. Derived from the accent so the
    /// whole UI follows the leaf-green primary color.
    var accentBackground: Color {
        switch effectiveTier {
        case .system: return accent.opacity(0.10)
        case .dark: return accent.opacity(0.16)
        case .highContrast: return accent.opacity(0.32)
        }
    }

    /// Accent color (also used for selected text, badges and primary buttons).
    /// A natural leaf green — replaces the system default blue (and the former
    /// high-contrast gold).
    var accent: Color {
        Color(red: 0.30, green: 0.62, blue: 0.34)
    }

    /// Primary text color.
    var textPrimary: Color {
        switch effectiveTier {
        case .system, .dark: return .primary
        case .highContrast: return .white
        }
    }

    /// Secondary text color.
    var textSecondary: Color {
        switch effectiveTier {
        case .system: return .secondary
        case .dark: return Color(red: 0.68, green: 0.68, blue: 0.70)
        case .highContrast: return Color(red: 0.85, green: 0.85, blue: 0.85)
        }
    }

    /// Tertiary / muted text color.
    var textTertiary: Color {
        switch effectiveTier {
        case .system: return Color.secondary.opacity(0.6)
        case .dark: return Color(red: 0.45, green: 0.45, blue: 0.48)
        case .highContrast: return Color(red: 0.70, green: 0.70, blue: 0.70)
        }
    }

    /// Graph edge / connector line color.
    var graphEdge: Color {
        switch effectiveTier {
        case .system: return Color(red: 0.6, green: 0.6, blue: 0.6)
        case .dark: return Color(red: 0.55, green: 0.55, blue: 0.60)
        case .highContrast: return Color.white
        }
    }

    /// Graph node card background.
    var nodeBackground: Color {
        switch effectiveTier {
        case .system: return Color(NSColor.controlBackgroundColor)
        case .dark: return Color(red: 0.15, green: 0.15, blue: 0.17)
        case .highContrast: return Color(red: 0.05, green: 0.05, blue: 0.05)
        }
    }

    /// Graph node border.
    var nodeBorder: Color {
        switch effectiveTier {
        case .system: return Color(red: 0.85, green: 0.85, blue: 0.85)
        case .dark: return Color(red: 0.70, green: 0.70, blue: 0.72)
        case .highContrast: return .white
        }
    }

    /// Chat receiver bubble background (for assistant messages).
    var chatSpeakerBackground: Color {
        switch effectiveTier {
        case .system: return Color.gray.opacity(0.3)
        case .dark: return Color(red: 0.23, green: 0.23, blue: 0.26)
        case .highContrast: return Color(red: 0.20, green: 0.20, blue: 0.20)
        }
    }

    /// Non-accent badge background (e.g. "Cargo" language badge).
    var badgeBackground: Color {
        switch effectiveTier {
        case .system: return Color.secondary.opacity(0.15)
        case .dark: return Color(red: 0.25, green: 0.25, blue: 0.28)
        case .highContrast: return Color(red: 0.35, green: 0.35, blue: 0.35)
        }
    }

    /// File icon tint for a source extension. Brighter in high-contrast mode
    /// so icons stand out against black.
    func fileIconColor(for ext: String) -> Color {
        switch ext.lowercased() {
        case "swift":
            return effectiveTier == .highContrast ? Color(red: 1.0, green: 0.6, blue: 0.2) : .orange
        case "rs":
            return effectiveTier == .highContrast
                ? Color(red: 1.0, green: 0.4, blue: 0.4)
                : Color(red: 0.87, green: 0.24, blue: 0.24)
        default:
            return textSecondary
        }
    }

    // MARK: - Window appearance

    /// Apply the effective NSAppearance to all application windows.
    /// `.system` resets to nil (follow the system setting); `.dark` forces
    /// Dark Aqua; `.highContrast` forces High Contrast Dark Aqua.
    func applyWindowAppearance() {
        let nsAppearance: NSAppearance? = {
            switch effectiveTier {
            case .system: return nil
            case .dark: return NSAppearance(named: .darkAqua)
            case .highContrast: return NSAppearance(named: .accessibilityHighContrastDarkAqua)
            }
        }()
        for window in NSApp.windows {
            window.appearance = nsAppearance
        }
    }
}