fn main() {
    // `generate_context!` needs the frontend output directory to exist. A checkout that has
    // not run `pnpm build` yet gets a placeholder so the Rust side still compiles and tests.
    let dist = std::path::Path::new("../dist");
    if !dist.join("index.html").exists() {
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(
            dist.join("index.html"),
            "<!doctype html><title>Personal Present</title><p>Run <code>pnpm build</code> in apps/desktop.</p>\n",
        );
    }
    tauri_build::build()
}
