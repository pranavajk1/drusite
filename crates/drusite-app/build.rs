// SPDX-License-Identifier: AGPL-3.0-or-later

fn main() {
    // The app has exactly one command, `core`, which carries the whole Core API.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["core"])),
    )
    .expect("tauri-build failed");
}
