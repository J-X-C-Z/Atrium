// Ported from packages/contracts/src/chatAttachment.ts (T3 Code v0.1.11, MIT).
//! Chat attachments and the structured context that rides on a user message.
//!
//! Also ports `OrchestrationMessageContext` from
//! `packages/contracts/src/composerContext.ts`. Context records are kept as
//! raw JSON: their union is forward-compatible, readers drop records they do
//! not know, and the engine only carries them between the composer and the
//! provider.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::base::{MessageId, ThreadId, trim_non_empty};

pub const PROVIDER_SEND_TURN_MAX_INPUT_CHARS: usize = 120_000;
pub const PROVIDER_SEND_TURN_MAX_ATTACHMENTS: usize = 100;
pub const PROVIDER_SEND_TURN_MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_MAX_TOTAL_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/gif", "image/jpeg", "image/png", "image/webp"];

const CHAT_ATTACHMENT_ID_MAX_CHARS: usize = 128;

/// Whether the provider accepts an image of this media type.
pub fn is_provider_send_turn_supported_image_mime_type(mime_type: &str) -> bool {
    let lower = mime_type.to_ascii_lowercase();
    PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES.contains(&lower.as_str())
}

/// A screenshot a pasted image came from. The accessibility tree is kept as
/// raw JSON; it is display-only payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotSource {
    pub kind: String,
    pub captured_at: String,
    pub app_name: String,
    pub window_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessible_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_icon_data_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatImageAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SnapShotSource>,
}

/// A path-only file attachment, or pasted text saved as a file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFileAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PastedTextAttachmentSource>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastedTextAttachmentSource {
    pub kind: String,
}

/// An attachment of a type this build does not know, kept so older readers
/// can pass it through. It is not an image or a file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUnknownAttachment {
    #[serde(rename = "type")]
    pub type_: String,
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// T3's `ChatAttachment` union.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum ChatAttachment {
    Image(ChatImageAttachment),
    File(ChatFileAttachment),
    Unknown(ChatUnknownAttachment),
}

fn validate_attachment_id(id: &str) -> Result<(), String> {
    let trimmed = trim_non_empty(id).ok_or("an attachment id must not be blank")?;
    if trimmed.len() > CHAT_ATTACHMENT_ID_MAX_CHARS {
        return Err("an attachment id is too long".to_owned());
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("an attachment id may only hold letters, digits, `_` and `-`".to_owned());
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = trim_non_empty(name).ok_or("an attachment name must not be blank")?;
    if trimmed.len() > 255 {
        return Err("an attachment name is too long".to_owned());
    }
    Ok(())
}

fn validate_mime(mime_type: &str, image: bool) -> Result<(), String> {
    let trimmed = trim_non_empty(mime_type).ok_or("an attachment mime type must not be blank")?;
    if trimmed.len() > 100 {
        return Err("an attachment mime type is too long".to_owned());
    }
    if image && !trimmed.to_ascii_lowercase().starts_with("image/") {
        return Err("an image attachment needs an image/ mime type".to_owned());
    }
    Ok(())
}

impl ChatImageAttachment {
    fn validate(&self) -> Result<(), String> {
        validate_attachment_id(&self.id)?;
        validate_name(&self.name)?;
        validate_mime(&self.mime_type, true)?;
        if self.size_bytes > PROVIDER_SEND_TURN_MAX_IMAGE_BYTES {
            return Err("an image attachment is larger than the limit".to_owned());
        }
        Ok(())
    }
}

impl ChatFileAttachment {
    fn validate(&self) -> Result<(), String> {
        validate_attachment_id(&self.id)?;
        validate_name(&self.name)?;
        validate_mime(&self.mime_type, false)?;
        if !(1..=PROVIDER_SEND_TURN_MAX_FILE_BYTES).contains(&self.size_bytes) {
            return Err("a file attachment is outside the size limits".to_owned());
        }
        Ok(())
    }
}

impl ChatUnknownAttachment {
    fn validate(&self) -> Result<(), String> {
        let type_ = trim_non_empty(&self.type_).ok_or("an attachment type must not be blank")?;
        if type_.len() > 50 {
            return Err("an attachment type is too long".to_owned());
        }
        if type_ == "image" || type_ == "file" {
            return Err("a known attachment type must decode in full".to_owned());
        }
        validate_attachment_id(&self.id)?;
        validate_name(&self.name)?;
        validate_mime(&self.mime_type, false)?;
        Ok(())
    }
}

fn trimmed(value: &str) -> String {
    trim_non_empty(value).unwrap_or_default()
}

impl<'de> Deserialize<'de> for ChatAttachment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;
        let value = serde_json::Value::deserialize(deserializer)?;
        let type_ = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| D::Error::custom("an attachment needs a string `type`"))?;
        match type_.as_str() {
            "image" => {
                let mut attachment: ChatImageAttachment =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                attachment.validate().map_err(D::Error::custom)?;
                // T3's `TrimmedNonEmptyString` trims on decode.
                attachment.id = trimmed(&attachment.id);
                attachment.name = trimmed(&attachment.name);
                attachment.mime_type = trimmed(&attachment.mime_type);
                Ok(Self::Image(attachment))
            }
            "file" => {
                let mut attachment: ChatFileAttachment =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                attachment.validate().map_err(D::Error::custom)?;
                attachment.id = trimmed(&attachment.id);
                attachment.name = trimmed(&attachment.name);
                attachment.mime_type = trimmed(&attachment.mime_type);
                Ok(Self::File(attachment))
            }
            _ => {
                let mut attachment: ChatUnknownAttachment =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                attachment.validate().map_err(D::Error::custom)?;
                attachment.type_ = trimmed(&attachment.type_);
                attachment.id = trimmed(&attachment.id);
                attachment.name = trimmed(&attachment.name);
                attachment.mime_type = trimmed(&attachment.mime_type);
                Ok(Self::Unknown(attachment))
            }
        }
    }
}

impl Serialize for ChatAttachment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Image(attachment) => {
                super::base::serialize_tagged("image", attachment, serializer)
            }
            Self::File(attachment) => super::base::serialize_tagged("file", attachment, serializer),
            Self::Unknown(attachment) => attachment.serialize(serializer),
        }
    }
}

impl ChatAttachment {
    pub fn type_name(&self) -> &str {
        match self {
            Self::Image(_) => "image",
            Self::File(_) => "file",
            Self::Unknown(attachment) => &attachment.type_,
        }
    }

    pub fn size_bytes(&self) -> u64 {
        match self {
            Self::Image(attachment) => attachment.size_bytes,
            Self::File(attachment) => attachment.size_bytes,
            Self::Unknown(attachment) => attachment.size_bytes,
        }
    }

    pub fn mime_type(&self) -> &str {
        match self {
            Self::Image(attachment) => &attachment.mime_type,
            Self::File(attachment) => &attachment.mime_type,
            Self::Unknown(attachment) => &attachment.mime_type,
        }
    }
}

/// The per-message attachment limit message, or `None` when it fits.
pub fn get_provider_attachment_limit_error(attachments: &[ChatAttachment]) -> Option<&'static str> {
    if attachments.len() > PROVIDER_SEND_TURN_MAX_ATTACHMENTS {
        return Some("You can attach up to 100 files per message or question response.");
    }
    let image_bytes: u64 = attachments
        .iter()
        .filter(|attachment| {
            attachment.type_name() == "image"
                || is_provider_send_turn_supported_image_mime_type(attachment.mime_type())
        })
        .map(ChatAttachment::size_bytes)
        .sum();
    if image_bytes > PROVIDER_SEND_TURN_MAX_TOTAL_IMAGE_BYTES {
        return Some(
            "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once.",
        );
    }
    None
}

/// A guest message's attribution, added by the server beside the text the
/// agent reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestMessageAttribution {
    pub guest_message_id: String,
    pub name: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_id: Option<String>,
}

/// T3's `OrchestrationMessageContext`. `version` is 1; `records` are raw
/// forward-compatible context records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationMessageContext {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<GuestMessageAttribution>,
    #[serde(default)]
    pub records: Vec<serde_json::Value>,
}

/// The input of `PersistChatAttachmentsResult` and message intake. Ported for
/// the wire names; the engine does not persist attachments itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistChatAttachmentsInput {
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub attachments: Vec<ChatAttachment>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Ported from packages/contracts/src/chatAttachment.test.ts.
    #[test]
    fn tolerates_attachment_types_from_newer_builds() {
        let attachment: ChatAttachment = serde_json::from_value(json!({
            "type": "somethingnew",
            "id": "thread-1-00000000-0000-4000-8000-000000000003-glb",
            "name": "scene.glb",
            "mimeType": "model/gltf-binary",
            "sizeBytes": 12,
        }))
        .unwrap();
        assert_eq!(attachment.type_name(), "somethingnew");
        assert_eq!(
            serde_json::to_value(&attachment).unwrap()["type"],
            json!("somethingnew")
        );
    }

    #[test]
    fn rejects_malformed_known_attachment_types_instead_of_tolerating_them() {
        let base = json!({
            "id": "thread-1-00000000-0000-4000-8000-000000000003-pdf",
            "name": "report.pdf",
            "mimeType": "application/pdf",
        });
        let mut oversized = base.clone();
        oversized["type"] = json!("file");
        oversized["sizeBytes"] = json!(PROVIDER_SEND_TURN_MAX_FILE_BYTES + 1);
        assert!(serde_json::from_value::<ChatAttachment>(oversized).is_err());

        let mut bad_mime = base;
        bad_mime["type"] = json!("image");
        bad_mime["sizeBytes"] = json!(12);
        assert!(serde_json::from_value::<ChatAttachment>(bad_mime).is_err());
    }

    #[test]
    fn round_trips_image_and_file_attachments() {
        let image = json!({
            "type": "image",
            "id": "a-1",
            "name": "shot.png",
            "mimeType": "image/png",
            "sizeBytes": 10,
        });
        let file = json!({
            "type": "file",
            "id": "a-2",
            "name": "notes.txt",
            "mimeType": "text/plain",
            "sizeBytes": 10,
            "source": { "kind": "pasted-text" },
        });
        for value in [image, file] {
            let parsed: ChatAttachment = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        }
    }

    #[test]
    fn rejects_hostile_attachment_ids() {
        assert!(
            serde_json::from_value::<ChatAttachment>(json!({
                "type": "somethingnew",
                "id": "bad id",
                "name": "x",
                "mimeType": "application/x",
                "sizeBytes": 1,
            }))
            .is_err()
        );
    }

    #[test]
    fn supported_image_types_are_case_insensitive() {
        assert!(is_provider_send_turn_supported_image_mime_type("IMAGE/PNG"));
        assert!(!is_provider_send_turn_supported_image_mime_type(
            "application/pdf"
        ));
    }
}
