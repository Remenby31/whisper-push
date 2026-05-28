// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Onboarding",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "Onboarding",
            path: "Sources"
        )
    ]
)
