// SPDX-License-Identifier: AGPL-3.0-or-later

fn main() {
    // No commands for the webview: the app calls this plugin from Rust only.
    tauri_plugin::Builder::new(&[])
        .android_path("android")
        .ios_path("ios")
        .build();
}
