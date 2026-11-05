//! Embeds the app icon and version info in nook.exe, and names the
//! repository the updater checks.

fn main() {
    // The repository whose releases the updater checks (a fork that
    // publishes Linux packages sets its own).
    println!("cargo:rerun-if-env-changed=NOOK_RELEASES_REPO");
    let repo = std::env::var("NOOK_RELEASES_REPO")
        .ok()
        .filter(|r| r.split('/').count() == 2 && !r.contains(char::is_whitespace))
        .unwrap_or_else(|| "Geguchh024/nook".into());
    println!("cargo:rustc-env=NOOK_REPO={repo}");

    const ICON: &str = "../../assets/icon/nook.ico";
    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON)
        .set("ProductName", "Nook")
        .set("FileDescription", "Nook")
        .set("LegalCopyright", "MIT License");
    res.compile().expect("embedding the icon resource");
}
