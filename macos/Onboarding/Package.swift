// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Onboarding",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "Onboarding",
            path: "Sources",
            resources: [
                // Brand kit AppIcon, embedded so SwiftUI can load it via
                // `Image("AppIcon", bundle: .module)` for the wizard logo.
                .process("Resources/AppIcon.png")
            ]
        )
    ]
)
