use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ConversionError {
    #[error("field `{field}` is not supported by {target}: {reason}")]
    Unsupported {
        field: String,
        target: &'static str,
        reason: String,
    },
    #[error("request body is empty or has no usable messages")]
    EmptyConversation,
    #[error("invalid JSON in {context}: {detail}")]
    InvalidJson { context: String, detail: String },
}

impl ConversionError {
    pub fn unsupported(
        field: impl Into<String>,
        target: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            field: field.into(),
            target,
            reason: reason.into(),
        }
    }

    pub fn status_code(&self) -> u16 {
        400
    }
}
