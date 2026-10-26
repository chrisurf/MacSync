// swift-tools-version:5.10
// MacSync menu bar app. Built by scripts/build-app.sh into MacSync.app,
// which also contains the Rust binary `macsync` (daemon + CLI).
import PackageDescription

let package = Package(
    name: "MacSync",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(name: "MacSync", path: "Sources/MacSync")
    ]
)
