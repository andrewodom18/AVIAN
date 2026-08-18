use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const PAYLOAD_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeotagStatus {
    Applied,
    NoTelemetry,
    Failed,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadPosition {
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub altitude_msl_m: Option<f64>,
}

impl PayloadPosition {
    fn validate(self) -> Result<(), PayloadError> {
        if !self.latitude_deg.is_finite()
            || !self.longitude_deg.is_finite()
            || !(-90.0..=90.0).contains(&self.latitude_deg)
            || !(-180.0..=180.0).contains(&self.longitude_deg)
            || self.altitude_msl_m.is_some_and(|value| !value.is_finite())
        {
            return Err(PayloadError::InvalidPosition);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageManifest {
    pub schema_version: u16,
    pub image_id: Uuid,
    pub captured_at_ms: u64,
    pub sensor: String,
    pub media_type: String,
    pub byte_count: u64,
    pub sha256: String,
    pub imagery_ref: String,
    pub position: Option<PayloadPosition>,
    pub heading_deg: Option<f64>,
    pub geotag_status: GeotagStatus,
}

impl ImageManifest {
    pub fn validate(&self) -> Result<(), PayloadError> {
        if self.schema_version != PAYLOAD_SCHEMA_VERSION {
            return Err(PayloadError::UnsupportedSchema(self.schema_version));
        }
        if self.captured_at_ms == 0 || self.byte_count == 0 {
            return Err(PayloadError::InvalidImageMetadata);
        }
        validate_short_text(&self.sensor)?;
        if self.media_type != "image/jpeg" {
            return Err(PayloadError::InvalidMediaType);
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PayloadError::InvalidSha256);
        }
        validate_relative_ref(&self.imagery_ref)?;
        if let Some(position) = self.position {
            position.validate()?;
        }
        if self
            .heading_deg
            .is_some_and(|heading| !heading.is_finite() || !(0.0..360.0).contains(&heading))
        {
            return Err(PayloadError::InvalidHeading);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detection {
    pub schema_version: u16,
    pub detection_id: Uuid,
    pub observed_at_ms: u64,
    pub image_id: Option<Uuid>,
    pub label: String,
    pub confidence: Option<f64>,
    pub position: Option<PayloadPosition>,
}

impl Detection {
    pub fn validate(&self) -> Result<(), PayloadError> {
        if self.schema_version != PAYLOAD_SCHEMA_VERSION {
            return Err(PayloadError::UnsupportedSchema(self.schema_version));
        }
        if self.observed_at_ms == 0 {
            return Err(PayloadError::InvalidDetectionMetadata);
        }
        validate_short_text(&self.label)?;
        if self
            .confidence
            .is_some_and(|confidence| !confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
        {
            return Err(PayloadError::InvalidConfidence);
        }
        if let Some(position) = self.position {
            position.validate()?;
        }
        Ok(())
    }
}

fn validate_short_text(value: &str) -> Result<(), PayloadError> {
    if value.trim().is_empty() || value.len() > 128 || value.contains(['\0', '\n', '\r']) {
        return Err(PayloadError::InvalidText);
    }
    Ok(())
}

fn validate_relative_ref(value: &str) -> Result<(), PayloadError> {
    if value.is_empty() || value.len() > 512 || value.contains('\0') {
        return Err(PayloadError::UnsafeImageryReference);
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PayloadError::UnsafeImageryReference);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum PayloadError {
    #[error("unsupported payload schema version {0}")]
    UnsupportedSchema(u16),
    #[error("payload text must contain 1-128 safe characters")]
    InvalidText,
    #[error("image metadata requires a timestamp and nonzero byte count")]
    InvalidImageMetadata,
    #[error("image media type must be image/jpeg")]
    InvalidMediaType,
    #[error("image sha256 must be 64 hexadecimal characters")]
    InvalidSha256,
    #[error("imagery reference must be a safe relative path")]
    UnsafeImageryReference,
    #[error("payload position is invalid")]
    InvalidPosition,
    #[error("heading must be finite and in [0, 360)")]
    InvalidHeading,
    #[error("detection timestamp is required")]
    InvalidDetectionMetadata,
    #[error("confidence must be finite and in [0, 1]")]
    InvalidConfidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(reference: &str) -> ImageManifest {
        ImageManifest {
            schema_version: PAYLOAD_SCHEMA_VERSION,
            image_id: Uuid::nil(),
            captured_at_ms: 1,
            sensor: "pi".into(),
            media_type: "image/jpeg".into(),
            byte_count: 10,
            sha256: "a".repeat(64),
            imagery_ref: reference.into(),
            position: None,
            heading_deg: None,
            geotag_status: GeotagStatus::NoTelemetry,
        }
    }

    #[test]
    fn image_reference_cannot_escape_imagery_root() {
        assert!(manifest("2026/image.jpg").validate().is_ok());
        assert_eq!(
            manifest("../secret").validate(),
            Err(PayloadError::UnsafeImageryReference)
        );
        assert_eq!(
            manifest("/home/rolex/image.jpg").validate(),
            Err(PayloadError::UnsafeImageryReference)
        );
    }

    #[test]
    fn detection_confidence_is_bounded() {
        let detection = Detection {
            schema_version: PAYLOAD_SCHEMA_VERSION,
            detection_id: Uuid::nil(),
            observed_at_ms: 1,
            image_id: None,
            label: "vehicle".into(),
            confidence: Some(1.1),
            position: None,
        };
        assert_eq!(detection.validate(), Err(PayloadError::InvalidConfidence));
    }
}
