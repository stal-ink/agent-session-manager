use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 更新签名公钥在编译期注入；环境变量变化必须触发重新编译。
    println!("cargo:rerun-if-env-changed=ASM_UPDATE_PUBKEY");
    slint_build::compile("ui/app.slint").unwrap();
    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets = root.join("assets");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app_icon.o");
    let status = Command::new("windres")
        .current_dir(&assets)
        .args(["-O", "coff", "app.rc", "-o"])
        .arg(&out)
        .status()
        .expect("windres is required to embed the application icon");
    assert!(status.success(), "windres failed");
    println!("cargo:rustc-link-arg-bins={}", out.display());
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/app.rc");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DEBUG:NONE");
    }
}
