// Ported from packages/contracts/src/baseSchemas.ts (T3 Code v0.1.11, MIT).
//! Entity ids and the small shared pieces of the orchestration contracts.
//!
//! Every id is a non-blank, trimmed string on the wire, exactly as T3's
//! `TrimmedNonEmptyString` brand encodes it. `UnknownUnionMember` mirrors
//! T3's decode-only fallback for a tagged union member this build does not
//! know: it keeps the raw JSON so a caller can skip the member, and it never
//! serializes back to the wire.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// T3's `IsoDateTime`: a JSON string. Timestamps stay strings here so a
/// decode/encode round trip is byte-for-byte the value that arrived.
pub type IsoDateTime = String;
/// T3's `NonNegativeInt`.
pub type NonNegativeInt = u32;
/// T3's `PositiveInt`.
pub type PositiveInt = u32;

/// T3's `TrimmedString` decode step: the trimmed value, or the original when
/// nothing changed. Returns `None` for a blank string.
pub fn trim_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.len() == value.len() {
        Some(value.to_owned())
    } else {
        Some(trimmed.to_owned())
    }
}

/// A decode-only member of a tagged union from a newer peer.
///
/// T3's `UnknownUnionMember` class: `tag` is the value of the discriminator
/// and `value` is the member's complete raw JSON. Encoding one fails, so an
/// unknown member is never forwarded as if it were known.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownUnionMember {
    pub tag: String,
    pub value: serde_json::Value,
}

impl UnknownUnionMember {
    pub fn new(tag: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            tag: tag.into(),
            value,
        }
    }
}

/// A member of a tagged union could not be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownMemberEncodingError {
    pub tag: String,
}

impl std::fmt::Display for UnknownMemberEncodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown {} values are never sent", self.tag)
    }
}

impl std::error::Error for UnknownMemberEncodingError {}

/// Serialize a known member with its discriminator. Members are structs and
/// always encode to a JSON object; the tag is added beside their fields.
pub(crate) fn serialize_tagged<T, S>(tag: &str, value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    serialize_tagged_with("type", tag, value, serializer)
}

/// Serialize a known member with a named discriminator. Members are structs
/// and always encode to a JSON object; the tag is added beside their fields.
pub(crate) fn serialize_tagged_with<T, S>(
    tag_field: &str,
    tag: &str,
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    let mut map = match serde_json::to_value(value).map_err(serde::ser::Error::custom)? {
        serde_json::Value::Object(map) => map,
        _ => {
            return Err(serde::ser::Error::custom(
                "a tagged union member must serialize to an object",
            ));
        }
    };
    map.insert(
        tag_field.to_owned(),
        serde_json::Value::String(tag.to_owned()),
    );
    map.serialize(serializer)
}

/// Read the string discriminator of a tagged union member.
pub(crate) fn tagged_union_tag<'de, D>(
    deserializer: D,
) -> Result<(String, serde_json::Value), D::Error>
where
    D: Deserializer<'de>,
{
    tagged_union_tag_with(deserializer, "type")
}

pub(crate) fn tagged_union_tag_with<'de, D>(
    deserializer: D,
    tag_field: &str,
) -> Result<(String, serde_json::Value), D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let tag = value
        .get(tag_field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| D::Error::custom("a tagged union member needs a string discriminator"))?;
    Ok((tag, value))
}

macro_rules! entity_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Trim and keep the id. An id's non-blank check happens on decode,
            /// as T3's schema does; constructors in the engine come from data
            /// that already passed it.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into().trim().to_owned())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self(String::new())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        // The orchestration core keeps ids opaque but still compares them
        // against the string keys it reads from the store.
        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.0.trim())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                trim_non_empty(&raw)
                    .map(Self)
                    .ok_or_else(|| D::Error::custom(concat!(stringify!($name), " must not be blank")))
            }
        }
    };
}

/// A tagged union whose members are structs. An unknown tag is an error: a
/// command or an artifact this build cannot read must not be acted on.
macro_rules! closed_union {
    ($union:ident, $($tag:literal => $variant:ident($ty:ty)),+ $(,)?) => {
        impl serde::Serialize for $union {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $(Self::$variant(value) => $crate::contracts::base::serialize_tagged($tag, value, serializer),)+
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $union {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let (tag, value) = $crate::contracts::base::tagged_union_tag(deserializer)?;
                match tag.as_str() {
                    $($tag => serde_json::from_value::<$ty>(value)
                        .map(Self::$variant)
                        .map_err(serde::de::Error::custom),)+
                    other => Err(serde::de::Error::custom(format!(
                        concat!("unknown ", stringify!($union), " member `{}`"), other
                    ))),
                }
            }
        }
    };
}

/// A tagged union with a decode-only unknown member, as T3's
/// `ForwardCompatibleUnion` produces. The unknown member keeps its raw JSON
/// and never serializes.
macro_rules! open_union {
    ($union:ident, $unknown:ident, $($tag:literal => $variant:ident($ty:ty)),+ $(,)?) => {
        impl serde::Serialize for $union {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $(Self::$variant(value) => $crate::contracts::base::serialize_tagged($tag, value, serializer),)+
                    Self::$unknown(value) => Err(serde::ser::Error::custom(format!(
                        "unknown {} values are never sent",
                        value.tag
                    ))),
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $union {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let (tag, value) = $crate::contracts::base::tagged_union_tag(deserializer)?;
                match tag.as_str() {
                    $($tag => serde_json::from_value::<$ty>(value)
                        .map(Self::$variant)
                        .map_err(serde::de::Error::custom),)+
                    _ => Ok(Self::$unknown($crate::contracts::base::UnknownUnionMember::new(tag, value))),
                }
            }
        }
    };
}

pub(crate) use closed_union;
pub(crate) use open_union;

entity_id!(ThreadId);
entity_id!(ProjectId);
entity_id!(EnvironmentId);
entity_id!(CommandId);
entity_id!(EventId);
entity_id!(MessageId);
entity_id!(TurnId);
entity_id!(RunId);
entity_id!(RunAttemptId);
entity_id!(NodeId);
entity_id!(ProviderItemId);
entity_id!(ProviderSessionId);
entity_id!(ProviderThreadId);
entity_id!(ProviderTurnId);
entity_id!(TurnItemId);
entity_id!(RuntimeRequestId);
entity_id!(ApprovalRequestId);
entity_id!(CheckpointRef);
entity_id!(CheckpointId);
entity_id!(CheckpointScopeId);
entity_id!(ContextHandoffId);
entity_id!(ContextTransferId);
entity_id!(PlanId);
entity_id!(RawEventId);
entity_id!(ScheduledTaskId);
entity_id!(GuestShareId);
entity_id!(GuestMessageId);
entity_id!(SecretRef);
entity_id!(ProviderDriverKind);
entity_id!(ProviderInstanceId);

/// T3's `ClientSurface`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientSurface {
    Web,
    Desktop,
    Mobile,
    Cli,
}

/// T3's `OrchestrationV2Actor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Agent,
    System,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_trim_on_both_directions_and_reject_blank() {
        let id: ThreadId = serde_json::from_str("\"  thread-1  \"").unwrap();
        assert_eq!(id.as_str(), "thread-1");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"thread-1\"");
        assert!(serde_json::from_str::<ThreadId>("\"   \"").is_err());
        assert!(serde_json::from_str::<ThreadId>("null").is_err());
    }

    #[test]
    fn actor_wire_values_are_lowercase() {
        assert_eq!(serde_json::to_string(&Actor::Agent).unwrap(), "\"agent\"");
        assert_eq!(
            serde_json::from_str::<Actor>("\"system\"").unwrap(),
            Actor::System
        );
    }
}
