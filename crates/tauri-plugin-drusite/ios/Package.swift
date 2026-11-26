// swift-tools-version:5.5
// SPDX-License-Identifier: AGPL-3.0-or-later

import PackageDescription

let package = Package(
    name: "tauri-plugin-drusite",
    platforms: [
        .macOS(.v10_13),
        .iOS(.v15),
    ],
    products: [
        .library(name: "tauri-plugin-drusite", type: .static, targets: ["tauri-plugin-drusite"]),
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api"),
    ],
    targets: [
        .target(
            name: "tauri-plugin-drusite",
            dependencies: [.byName(name: "Tauri")],
            path: "Sources"),
    ]
)
