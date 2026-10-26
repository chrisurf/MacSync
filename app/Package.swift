// swift-tools-version:5.10
// RepoSync menu bar app. Built by scripts/build-app.sh into RepoSync.app,
// which also contains the Rust binary `reposync` (daemon + CLI).
import PackageDescription

let package = Package(
    name: "RepoSync",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(name: "RepoSync", path: "Sources/RepoSync")
    ]
)
