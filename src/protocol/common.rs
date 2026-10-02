use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::ser::SerializeMap;

/// An insertion-ordered JSON object backed by a contiguous array of entries.
///
/// Protocol request maps are only traversed after parsing, so this deliberately
/// exposes iteration rather than key-based lookup or mutation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArrayMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> ArrayMap<K, V> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }
}

impl<K, V, const N: usize> From<[(K, V); N]> for ArrayMap<K, V> {
    fn from(entries: [(K, V); N]) -> Self {
        Self {
            entries: Vec::from(entries),
        }
    }
}

impl<K, V> FromIterator<(K, V)> for ArrayMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

impl<'a, K, V> IntoIterator for &'a ArrayMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = ArrayMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        ArrayMapIter {
            inner: self.entries.iter(),
        }
    }
}

pub struct ArrayMapIter<'a, K, V> {
    inner: std::slice::Iter<'a, (K, V)>,
}

impl<'a, K, V> Iterator for ArrayMapIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for ArrayMapIter<'_, K, V> {}

impl<K, V> Serialize for ArrayMap<K, V>
where
    K: Serialize,
    V: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, K, V> Deserialize<'de> for ArrayMap<K, V>
where
    K: Deserialize<'de> + PartialEq,
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ArrayMapVisitor<K, V>(std::marker::PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for ArrayMapVisitor<K, V>
        where
            K: Deserialize<'de> + PartialEq,
            V: Deserialize<'de>,
        {
            type Value = ArrayMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((key, value)) = map.next_entry::<K, V>()? {
                    if entries.iter().any(|(existing, _)| existing == &key) {
                        return Err(de::Error::custom("duplicate key in request object"));
                    }
                    entries.push((key, value));
                }
                Ok(ArrayMap { entries })
            }
        }

        deserializer.deserialize_map(ArrayMapVisitor(std::marker::PhantomData))
    }
}

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

#[cfg(test)]
mod tests {
    use super::ArrayMap;

    #[test]
    fn preserves_json_object_order_and_round_trips() {
        let map: ArrayMap<String, u8> =
            serde_json::from_str(r#"{"B":2,"A":1}"#).expect("array map");
        assert_eq!(
            map.iter()
                .map(|(key, value)| (key.as_str(), *value))
                .collect::<Vec<_>>(),
            [("B", 2), ("A", 1)]
        );
        assert_eq!(serde_json::to_string(&map).unwrap(), r#"{"B":2,"A":1}"#);
    }

    #[test]
    fn rejects_duplicate_json_keys() {
        let error = serde_json::from_str::<ArrayMap<String, u8>>(r#"{"A":1,"A":2}"#)
            .expect_err("duplicate key");
        assert!(error.to_string().contains("duplicate key"));
    }
}
