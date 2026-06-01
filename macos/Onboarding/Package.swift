// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Onboarding",
    // macOS 14 (Sonoma, 2023) — needed for `.scrollBounceBehavior(.basedOnSize)`
    // in ModelPickerView. Well below the install base in 2026.
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "Onboarding",
            path: "Sources",
            resources: [
                // Brand kit AppIcon as PDF (vector) — crisp at every size
                // the wizard uses. The PNG is kept around as a fallback and
                // for the AppDelegate's `NSApp.applicationIconImage`.
                .process("Resources/AppIcon.pdf"),
                .process("Resources/AppIcon.png")
            ]
        )
    ]
)
