use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?);
    let directory = root.join("assets/levels");
    println!("cargo:rerun-if-changed=assets/levels");
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "svg"));
    paths.sort();
    let mut source = String::from("&[\n");
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("level filename must be UTF-8")?;
        let absolute = path.to_str().ok_or("level path must be UTF-8")?;
        source.push_str(&format!("({name:?}, include_str!({absolute:?})),\n"));
    }
    source.push_str("]\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing build output directory")?);
    fs::write(output.join("bundled_levels.rs"), source)?;
    Ok(())
}
