// swift-tools-version: 6.2
import PackageDescription

// The Mac app.
//
// SwiftPM rather than an .xcodeproj so the whole project stays text and diffable. Xcode opens
// this package directly. `scripts/build-app.sh` wraps the executable into a .app bundle with
// LSUIElement set, which is what makes it a menu bar app with no Dock icon.
//
// LokiCore is a system library target that links the Rust static library built by Cargo.

let package = Package(
    name: "LokiApp",
    platforms: [.macOS(.v26)],
    targets: [
        .systemLibrary(name: "CLoki", path: "Sources/CLoki"),
        .target(
            name: "LokiCore",
            dependencies: ["CLoki"],
            linkerSettings: [
                .unsafeFlags(["-L../target/debug", "-L../target/release"]),
                .linkedLibrary("loki_ffi"),
            ]
        ),
        .executableTarget(name: "LokiApp", dependencies: ["LokiCore"]),
    ]
)
