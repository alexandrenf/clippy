// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "ClippySyncCore",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [
        .library(name: "ClippySyncCore", targets: ["ClippySyncCore"])
    ],
    targets: [
        .target(name: "ClippySyncCore", path: "Shared"),
        .testTarget(
            name: "ClippySyncCoreTests",
            dependencies: ["ClippySyncCore"],
            path: "Tests"
        )
    ]
)
