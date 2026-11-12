fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    // The version resource follows the workspace version, so Settings → Apps and the file's
    // Properties show the same number as the release.
    let version = env!("CARGO_PKG_VERSION");
    let mut parts: Vec<u32> = version.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let comma = parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let out = std::env::var("OUT_DIR").unwrap();
    let rc = std::fs::read_to_string("res/app.rc").unwrap().replace("@VERSION_COMMA@", &comma).replace("@VERSION@", version);
    let res_dir = std::path::Path::new(&out).join("res");
    std::fs::create_dir_all(&res_dir).unwrap();
    for f in ["app.ico", "app.manifest"] {
        std::fs::copy(format!("res/{f}"), res_dir.join(f)).unwrap();
    }
    let rc_path = res_dir.join("app.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
}
