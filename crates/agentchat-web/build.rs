//! Refuse absent/stale assets before compiling immutable bytes into the binary.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    node: String,
    inputs: BTreeMap<String, String>,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    mime: String,
    bytes: usize,
    sha256: String,
}
const DIRECTORIES: &[&str] = &[
    "web/src",
    "web/public",
    "web/graphql",
    "web/fixtures",
    "web/scripts",
    "web/licenses",
];
const INPUTS: &[&str] = &[
    "Cargo.toml",
    "web/package.json",
    "web/package-lock.json",
    "web/tsconfig.json",
    "web/vite.config.ts",
    "web/codegen.ts",
    "web/index.html",
    "web/chigiri-provenance.json",
    "chigiri.toml",
    "generated/agentchat-viewer-contract/chigiri-generated.json",
];

fn main() {
    if let Err(error) = prepare() {
        panic!(
            "Embedded viewer assets are missing, stale or invalid: {error}. Run `npm ci --prefix web && npm run build --prefix web` (or `make build`) before standalone Cargo commands."
        );
    }
}
fn prepare() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("manifest directory")?)
        .join("../..")
        .canonicalize()?;
    let manifest_path = root.join("web/dist/agentchat-assets.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let bytes = fs::read(&manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 || !manifest.node.starts_with('v') {
        return Err("asset schema/build identity".into());
    }
    let mut expected = BTreeSet::new();
    for directory in DIRECTORIES {
        println!("cargo:rerun-if-changed={}", root.join(directory).display());
        walk(&root, &root.join(directory), &mut expected)?;
    }
    expected.extend(INPUTS.iter().map(|name| name.to_string()));
    if expected != manifest.inputs.keys().cloned().collect() {
        return Err("asset input inventory mismatch".into());
    }
    for (name, expected) in &manifest.inputs {
        let path = file(&root, name)?;
        println!("cargo:rerun-if-changed={}", path.display());
        if digest(&fs::read(path)?) != *expected {
            return Err(format!("changed input {name}").into());
        }
    }
    let directory = root.join("web/dist");
    let mut present = BTreeSet::new();
    walk(&directory, &directory, &mut present)?;
    present.remove("agentchat-assets.json");
    let names: BTreeSet<_> = manifest
        .assets
        .iter()
        .map(|asset| asset.path.clone())
        .collect();
    if names.len() != manifest.assets.len()
        || names != present
        || !names.contains("index.html")
        || !names.contains("third-party-notices.txt")
        || !names.contains("build-info.txt")
        || names.len() > 64
    {
        return Err("asset ownership inventory mismatch".into());
    }
    let mut total = 0;
    let mut generated = String::from(
        "// Embedded assets, verified by build.rs.\npub(super) const ASSETS: &[Asset] = &[\n",
    );
    for asset in &manifest.assets {
        let path = file(&directory, &asset.path)?;
        println!("cargo:rerun-if-changed={}", path.display());
        let contents = fs::read(&path)?;
        if asset.bytes != contents.len()
            || asset.bytes > 1024 * 1024
            || digest(&contents) != asset.sha256
        {
            return Err(format!("changed/oversized asset {}", asset.path).into());
        }
        if mime(&asset.path) != Some(asset.mime.as_str()) {
            return Err("asset MIME mismatch".into());
        }
        total += asset.bytes;
        let uri = if asset.path == "index.html" {
            "/".to_owned()
        } else {
            format!("/{}", asset.path)
        };
        generated.push_str(&format!("Asset {{ path: {uri:?}, mime: {:?}, sha256: {:?}, bytes: include_bytes!({path:?}) }},\n", asset.mime, asset.sha256));
    }
    generated.push_str("];\n");
    if total > 8 * 1024 * 1024 || generated.lines().count() > 400 {
        return Err("embedded source/byte budget".into());
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("output directory")?);
    fs::write(out.join("assets.rs"), generated)?;
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn file(root: &Path, name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if !Path::new(name)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err("invalid input/asset path".into());
    }
    let path = root.join(name);
    if !fs::symlink_metadata(&path)?.is_file() || path.canonicalize()? != path {
        return Err("nonregular or escaped asset".into());
    }
    Ok(path)
}
fn walk(
    root: &Path,
    directory: &Path,
    output: &mut BTreeSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err("symlink in inputs/assets".into());
        }
        if kind.is_dir() {
            walk(root, &entry.path(), output)?;
        } else if kind.is_file() {
            output.insert(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or("non-UTF8 asset")?
                    .to_owned(),
            );
        } else {
            return Err("nonregular input/asset".into());
        }
    }
    Ok(())
}
fn mime(path: &str) -> Option<&'static str> {
    match Path::new(path).extension()?.to_str()? {
        "html" => Some("text/html; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "woff2" => Some("font/woff2"),
        "txt" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}
