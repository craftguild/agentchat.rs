//! Inline provenance preserves flat object payloads consumed by native JavaScript.
use super::{
    Budget,
    directory::{Directory, FileId},
    encoded, required,
};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

const KEY: &str = "_agentchat_metadata";
const VALUE: &str = "_agentchat_value";
pub(super) struct Decoded {
    pub value: Value,
    pub proved: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u32,
    parent: FileId,
    file: FileId,
    name: Vec<u8>,
    object: bool,
    checksum: [u8; 32],
}
pub(super) fn payload(value: &impl Serialize, budget: Budget) -> Result<Value> {
    let value: Value = serde_json::from_slice(&encoded(value, budget)?).map_err(|_| required())?;
    if value.as_object().is_some_and(|map| map.contains_key(KEY)) {
        return Err(required());
    }
    Ok(value)
}
fn frame(parent: FileId, file: FileId, name: &OsStr, payload: Value) -> Result<Value> {
    let object = payload.is_object();
    let mut frame = match payload {
        Value::Object(map) => Value::Object(map),
        scalar => serde_json::json!({VALUE: scalar}),
    };
    let proof = Proof {
        version: 1,
        parent,
        file,
        name: name.as_bytes().into(),
        object,
        checksum: [0; 32],
    };
    frame.as_object_mut().ok_or_else(required)?.insert(
        KEY.into(),
        serde_json::to_value(proof).map_err(|_| required())?,
    );
    Ok(frame)
}
fn checksum(frame: &Value, budget: Budget) -> Result<[u8; 32]> {
    let mut original = frame.clone();
    original[KEY]["checksum"] = serde_json::to_value([0u8; 32]).map_err(|_| required())?;
    Ok(Sha256::new()
        .chain_update(b"agentchat/native-metadata/v1\0")
        .chain_update(encoded(&original, budget)?)
        .finalize()
        .into())
}
pub(super) fn check_size(
    parent: &Directory,
    name: &OsStr,
    payload: &Value,
    budget: Budget,
) -> Result<()> {
    // Bound the complete representation before creating a filesystem stage.
    // Max-width inode/digest numbers dominate the actual later encoding.
    let mut maximum = frame(parent.id, FileId(u64::MAX, u64::MAX), name, payload.clone())?;
    maximum[KEY]["checksum"] = serde_json::to_value([255u8; 32]).map_err(|_| required())?;
    encoded(&maximum, budget).map(|_| ())
}
pub(super) fn encode(
    parent: &Directory,
    file: FileId,
    name: &OsStr,
    payload: Value,
    budget: Budget,
) -> Result<Vec<u8>> {
    let mut value = frame(parent.id, file, name, payload)?;
    value[KEY]["checksum"] =
        serde_json::to_value(checksum(&value, budget)?).map_err(|_| required())?;
    encoded(&value, budget)
}
pub(super) fn decode(
    parent: FileId,
    file: FileId,
    name: &OsStr,
    bytes: &[u8],
    proved: bool,
    budget: Budget,
) -> Result<Decoded> {
    let mut value: Value = serde_json::from_slice(bytes).map_err(|_| required())?;
    let Some(raw) = value.get(KEY) else {
        return if proved {
            Err(required())
        } else {
            Ok(Decoded {
                value,
                proved: false,
            })
        };
    };
    let proof: Proof = serde_json::from_value(raw.clone()).map_err(|_| required())?;
    // A checksum over parsed JSON alone could hide duplicate fields or added
    // whitespace. Proved records retain the writer's exact canonical encoding.
    if encoded(&value, budget)? != bytes {
        return Err(required());
    }
    if proof.version != 1
        || proof.parent != parent
        || proof.file != file
        || proof.name != name.as_bytes()
        || proof.checksum != checksum(&value, budget)?
    {
        return Err(required());
    }
    let map = value.as_object_mut().ok_or_else(required)?;
    map.remove(KEY);
    if proof.object {
        return Ok(Decoded {
            value,
            proved: true,
        });
    }
    if map.len() != 1 {
        return Err(required());
    }
    Ok(Decoded {
        value: map.remove(VALUE).ok_or_else(required)?,
        proved: true,
    })
}
