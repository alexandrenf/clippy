// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "ClippySyncCore",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [
        .library(name: "ClippySyncCore", targets: ["ClippySyncCore"])
    ],
    dependencies: [
        .package(url: "https://github.com/get-convex/convex-swift", from: "0.8.1")
    ],
    targets: [
        .target(
            name: "ClippySyncCore",
            dependencies: [.product(name: "ConvexMobile", package: "convex-swift")],
            path: "Shared"
        ),
        .testTarget(
            name: "ClippySyncCoreTests",
            dependencies: ["ClippySyncCore"],
            path: "Tests"
        )
    ]
)
