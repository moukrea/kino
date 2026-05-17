// Tauri build script. In addition to the standard tauri-build invocation,
// captures the current git commit SHA and exposes it as `KINO_COMMIT_SHA`
// to the runtime via `env!()`. The `get_app_info` Tauri command surfaces
// it on the F-016 Settings → About screen.
//
// PRD §F-016 §8 About: "Commit SHA (build-time injected)".
//
// The SHA is read from `git rev-parse HEAD`. Failure (no git, detached
// build context, shallow clone with no HEAD ref) falls back to the
// literal `"unknown"` so the build never breaks on a release tarball.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KINO_COMMIT_SHA={sha}");
    // Re-run the build script when HEAD or the active ref changes so the
    // injected SHA matches whatever's checked out at build time. The
    // `git/HEAD` symref points at the ref under `refs/heads/<branch>`,
    // which tracks the commit being built.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/index");

    tauri_build::build();
}
