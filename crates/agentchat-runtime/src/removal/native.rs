//! Lossless journal paths never depend on their diagnostic display strings.
use serde::{Deserialize, Deserializer, Serializer};
use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};
pub(super) fn serialize<S: Serializer>(
    path: &Path,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_bytes(path.as_os_str().as_encoded_bytes())
}
pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<PathBuf, D::Error> {
    let bytes = Vec::<u8>::deserialize(deserializer)?;
    if bytes.len() > 16384 || bytes.contains(&0) {
        return Err(serde::de::Error::custom("Invalid native removal path."));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}
