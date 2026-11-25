use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, Error as _},
};

use super::{Container, ContainerId, FileName, FolderPath, RecycleId, Recycled, Result, invalid};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerCursor {
    pub folder: FolderPath,
    pub name: FileName,
    pub id: ContainerId,
}

impl ContainerCursor {
    pub fn encode(container: &Container) -> Result<String> {
        encode(&Self {
            folder: container.folder.clone(),
            name: container.name.clone(),
            id: container.id.clone(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecycleCursor {
    pub at: u64,
    pub id: RecycleId,
}

impl RecycleCursor {
    pub fn encode(entry: &Recycled) -> Result<String> {
        encode(&Self {
            at: entry.recycle.recycled_at,
            id: entry.recycle.recycle_id.clone(),
        })
    }
}

pub fn decode<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    input: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<String>::deserialize(input)?
        .map(|value| decode_value(&value).map_err(D::Error::custom))
        .transpose()
}

fn decode_value<T: DeserializeOwned>(value: &str) -> Result<T> {
    if value.len() > 8192 {
        return Err(invalid("invalid cursor"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid("invalid cursor"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn encode(value: &impl Serialize) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?))
}
