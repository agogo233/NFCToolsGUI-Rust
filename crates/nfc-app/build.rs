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
    println!("cargo:rerun-if-changed=../../assets/icons/icon.ico");

    // Windows 下把 icon.ico 嵌入 exe 资源（文件图标 / 资源管理器缩略图）。
    // 非 Windows 平台是 no-op（winres 内部按 cfg 空实现）。
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("../../assets/icons/icon.ico");
        res.compile()
            .expect("failed to embed assets/icons/icon.ico into exe");
    }
}
