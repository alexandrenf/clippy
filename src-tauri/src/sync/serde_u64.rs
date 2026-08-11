use serde::de::{self, Visitor};
use serde::Deserializer;
use std::fmt;

const MAX_SAFE_JSON_INTEGER: u64 = (1_u64 << 53) - 1;

pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(U64Visitor)
}

struct U64Visitor;

impl Visitor<'_> for U64Visitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a non-negative integer encoded as a JSON integer or float")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value <= MAX_SAFE_JSON_INTEGER {
            Ok(value)
        } else {
            Err(E::custom("counter exceeds the JSON safe-integer limit"))
        }
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let value = u64::try_from(value).map_err(|_| E::custom("counter cannot be negative"))?;
        self.visit_u64(value)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_finite()
            && value >= 0.0
            && value.fract() == 0.0
            && value <= MAX_SAFE_JSON_INTEGER as f64
        {
            Ok(value as u64)
        } else {
            Err(E::custom("counter must be a finite non-negative integer"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::deserialize;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Counter {
        #[serde(deserialize_with = "deserialize")]
        value: u64,
    }

    #[test]
    fn accepts_convex_json_numbers() {
        assert_eq!(
            serde_json::from_str::<Counter>(r#"{"value":88.0}"#).unwrap(),
            Counter { value: 88 }
        );
        assert_eq!(
            serde_json::from_str::<Counter>(r#"{"value":88}"#).unwrap(),
            Counter { value: 88 }
        );
    }

    #[test]
    fn rejects_invalid_counters() {
        assert!(serde_json::from_str::<Counter>(r#"{"value":-1.0}"#).is_err());
        assert!(serde_json::from_str::<Counter>(r#"{"value":1.5}"#).is_err());
    }
}
