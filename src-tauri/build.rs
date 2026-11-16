use std::{fs, path::Path};

// The UI is the repo-root HTML/CSS/fonts. Copy just those into ui-dist so Tauri
// embeds them and nothing else (the repo root also holds node_modules, dist, ...).
fn copy_matching(from: &str, to: &Path, exts: &[&str]) {
    for entry in fs::read_dir(from).unwrap().flatten() {
        let p = entry.path();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if p.is_file() && exts.contains(&ext) {
            println!("cargo:rerun-if-changed={}", p.display());
            fs::copy(&p, to.join(p.file_name().unwrap())).unwrap();
        }
    }
}

fn main() {
    let out = Path::new("ui-dist");
    let _ = fs::remove_dir_all(out);
    fs::create_dir_all(out.join("fonts")).unwrap();
    copy_matching("..", out, &["html", "css"]);
    copy_matching("../fonts", &out.join("fonts"), &["woff2"]);
    tauri_build::build()
}
