// swift-tools-version: 6.2
// The Swift shell of devload. The Rust core is a static library built by
// `make core` (../core/target/release/libdevload_core.a); this package only
// draws the icon and the popover.
import PackageDescription
import Foundation

let coreLib = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .appendingPathComponent("../core/target/release")
    .standardizedFileURL.path

let package = Package(
    name: "Devload",
    platforms: [.macOS("26.0")],
    targets: [
        .systemLibrary(name: "CDevloadCore", path: "Sources/CDevloadCore"),
        .target(
            name: "DevloadKit",
            dependencies: ["CDevloadCore"],
            linkerSettings: [.unsafeFlags(["-L\(coreLib)"])]
        ),
        .executableTarget(name: "Devload", dependencies: ["DevloadKit"]),
        .testTarget(name: "DevloadKitTests", dependencies: ["DevloadKit"]),
    ]
)
