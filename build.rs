use std::process::Command;

// Keeps `target/` from growing without bound; see the module's own docs.
#[path = "build_support/target_gc.rs"]
mod target_gc;

fn main() {
    // Short commit hash, printed by every tool so a log or bug report
    // identifies the exact build. "unknown" when built outside a checkout.
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_HASH={hash}");

    // `.git/HEAD` alone is not enough: on a normal commit it keeps the same
    // contents ("ref: refs/heads/main") and only the ref file underneath it
    // moves, so watching HEAD by itself leaves the hash stuck at whatever it
    // was when the crate was first built. Watching the refs directory catches
    // commits; HEAD still catches branch switches.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=build.rs");
    // Also re-run on the changes that rebuild the crate anyway, so the
    // target-folder cleanup below gets its turn on every real rebuild.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=build_support");
    println!("cargo:rerun-if-env-changed=TARGET_GC");
    // `false`: this script's output only changes on a commit, so the crate's
    // own targets are not rebuilt on every run, and the cleanup must leave
    // their earlier builds alone.
    target_gc::run_from_build_script(false);
}
