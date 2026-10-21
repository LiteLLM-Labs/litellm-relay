// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "RelayBarGlass",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "RelayBarGlass",
            path: "Sources/RelayBarGlass"
        ),
        .testTarget(
            name: "RelayBarGlassTests",
            dependencies: ["RelayBarGlass"],
            path: "Tests/RelayBarGlassTests"
        ),
    ]
)
