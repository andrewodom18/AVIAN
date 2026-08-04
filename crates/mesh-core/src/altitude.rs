use serde::{Deserialize, Serialize};
use thiserror::Error;

/// System-wide operational ceiling: 25,000 feet MSL.
pub const SYSTEM_MAX_MSL_FT: f64 = 25_000.0;
/// Exact conversion of 25,000 feet to metres.
pub const SYSTEM_MAX_MSL_M: f64 = 7_620.0;

/// Altitudes retain their reference datum instead of conflating MSL, AGL,
/// and height above the launch point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Altitude {
    pub msl_m: f64,
    pub agl_m: f64,
    pub above_launch_m: f64,
}

impl Altitude {
    pub fn new(msl_m: f64, agl_m: f64, above_launch_m: f64) -> Result<Self, AltitudeError> {
        if !msl_m.is_finite() || !agl_m.is_finite() || !above_launch_m.is_finite() {
            return Err(AltitudeError::NonFinite);
        }
        if agl_m < 0.0 {
            return Err(AltitudeError::NegativeAgl(agl_m));
        }
        if msl_m > SYSTEM_MAX_MSL_M {
            return Err(AltitudeError::AboveSystemCeiling(msl_m));
        }

        Ok(Self {
            msl_m,
            agl_m,
            above_launch_m,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum AltitudeError {
    #[error("altitude values must be finite")]
    NonFinite,
    #[error("AGL altitude cannot be negative: {0} m")]
    NegativeAgl(f64),
    #[error("MSL altitude {0} m exceeds the system ceiling of 7,620 m")]
    AboveSystemCeiling(f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exact_system_ceiling() {
        assert!(Altitude::new(SYSTEM_MAX_MSL_M, 100.0, 100.0).is_ok());
    }

    #[test]
    fn rejects_altitude_above_system_ceiling() {
        assert_eq!(
            Altitude::new(SYSTEM_MAX_MSL_M + 0.1, 100.0, 100.0),
            Err(AltitudeError::AboveSystemCeiling(SYSTEM_MAX_MSL_M + 0.1))
        );
    }
}
