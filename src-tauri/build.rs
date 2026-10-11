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
    let mut images =
        String::from("pub fn image(name: &str) -> Option<&'static [u8]> { match name {\n");
    let mut files: Vec<_> = std::fs::read_dir("assets")
        .unwrap()
        .map(|file| file.unwrap().path())
        .collect();
    files.sort();
    for path in files {
        if path.extension().is_some_and(|s| s == "png") {
            images.push_str(&format!(
                "{:?} => Some(include_bytes!({:?})),\n",
                path.file_name().unwrap().to_str().unwrap(),
                path.canonicalize().unwrap().to_str().unwrap()
            ));
        }
    }
    images.push_str("_ => None }}");
    std::fs::write(
        std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("native-assets.rs"),
        images,
    )
    .unwrap();
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=annotate.rc");
    println!("cargo:rerun-if-changed=annotate.manifest");
    embed_resource::compile("annotate.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
