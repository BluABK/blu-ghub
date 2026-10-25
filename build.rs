use std::process::Command;

// The same drawing the app uses for its tray icon. Included rather than
// imported: a build script is its own crate and cannot reach into the one it
// is building, and drawing the icon twice would let the two drift apart.
#[cfg(windows)]
mod icon_art {
    include!("src/icon_art.rs");
}

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

    #[cfg(windows)]
    {
        embed_icon();
        embed_manifest::embed_manifest(embed_manifest::new_manifest("BluGHub"))
            .expect("embedding the application manifest");
    }
}

/// Put the icon inside the executable, where Explorer, the Start Menu and the
/// taskbar look for it. A missing resource compiler is a warning: it costs
/// the icon, not the build.
#[cfg(windows)]
fn embed_icon() {
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("BluGHub.ico");
    if let Err(e) = std::fs::write(&ico, icon_art::ico(icon_art::SIZES)) {
        println!("cargo:warning=could not write {}: {e}", ico.display());
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy());
    res.set("FileDescription", "BluGHub - battery, lighting and keys for a G915 and a G935");
    res.set("ProductName", "BluGHub");
    if let Err(e) = res.compile() {
        println!("cargo:warning=icon not embedded (no resource compiler?): {e}");
    }
}
