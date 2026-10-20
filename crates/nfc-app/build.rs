fn main() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=GIT_HASH={hash}");
    println!("cargo:rustc-env=APP_VERSION=v1.0.0");
    let builder = std::env::var("NFCTOOLSGUI_COMPILER")
        .unwrap_or_else(|_| std::env::var("USERNAME").unwrap_or_else(|_| "local".into()));
    println!("cargo:rustc-env=BUILDER={builder}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
