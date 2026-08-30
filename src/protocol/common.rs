use serde::Deserialize;
use serde::Deserializer;

pub fn deserialize_non_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;

    if value.trim().is_empty() {
        return Err(serde::de::Error::custom("string value must not be empty"));
    }

    Ok(value)
}
