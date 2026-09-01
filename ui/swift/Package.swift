// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "SpireUI",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "SpireUI", targets: ["SpireUI"]),
    ],
    dependencies: [
        // Syntax highlighting for the file viewer
        .package(url: "https://github.com/appstefan/HighlightSwift.git", branch: "main"),
        // Real VT100/xterm terminal emulator (the interactive shell renders
        // with \r, ESC[K/J, cursor + bracketed-paste codes — a text view can't
        // display that; SwiftTerm decodes it correctly).
        .package(url: "https://github.com/migueldeicaza/SwiftTerm.git", from: "1.2.0"),
    ],
    targets: [
        .executableTarget(
            name: "SpireUI",
            dependencies: [
                .product(name: "HighlightSwift", package: "HighlightSwift"),
                .product(name: "SwiftTerm", package: "SwiftTerm"),
            ],
            resources: [
                // libspire_code.dylib is NOT linked at load time: SpireFFI.swift
                // loads it via dlopen() — from the .app bundle's
                // Contents/Frameworks/ directory when bundled, or from
                // <repo-root>/target/release during development.
            ],
        ),
        .testTarget(
            name: "SpireUITests",
            dependencies: ["SpireUI"]
        ),
    ]
)