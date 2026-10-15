fn main() {
    for weight in [300, 400, 500, 700] {
        let font = format!("assets/fonts/Satoshi-{weight}.otf");
        assert!(
            std::path::Path::new(&font).is_file(),
            "Missing {font}. Run scripts/prepare-fonts.ps1 first."
        );
    }
    let config = std::fs::read("../src/oauth.config.json").unwrap_or_else(|_| b"{}".to_vec());
    std::fs::write(
        std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("oauth-config.json"),
        config,
    )
    .unwrap();
    println!("cargo:rerun-if-changed=../src/oauth.config.json");
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=annotate.rc");
    println!("cargo:rerun-if-changed=annotate.manifest");
    #[cfg(target_os = "windows")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("annotate.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
