//! Compile-time owned immutable assets; runtime never opens frontend source files.

pub(crate) struct Asset {
    pub path: &'static str,
    pub mime: &'static str,
    pub sha256: &'static str,
    pub bytes: &'static [u8],
}
include!(concat!(env!("OUT_DIR"), "/assets.rs"));

pub(crate) fn get(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.path == path)
}
