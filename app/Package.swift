// swift-tools-version: 6.2
import PackageDescription

// The Mac app.
//
// SwiftPM rather than an .xcodeproj so the whole project stays text and diffable. Xcode opens this
// package directly, which gives the editor, debugger, previews and Instruments with no project
// file to merge.
//
// Resources/Info.plist is embedded into the executable with -sectcreate, so LSUIElement applies
// when the binary runs on its own. Without that, `swift run` and Xcode's Run both show a Dock
// icon while the assembled bundle does not, and the two behave differently for no good reason.
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
        .executableTarget(
            name: "LokiApp",
            dependencies: ["LokiCore"],
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", "Resources/Info.plist",
                ])
            ]
        ),
    ]
)
