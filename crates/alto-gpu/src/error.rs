//! Renderer failures by kind. Display gives the SDK's `CODE: detail` form, so the
//! Worker's classification of a message and a match on the variant agree.
use alto_raster::TileError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    InvalidArgument(String),
    Conflict(String),
    CorruptData(String),
    /// The device is gone; the caller recreates the renderer.
    GpuLost(String),
    GpuUnsupported(String),
    /// What this frame or stroke needs does not fit the GPU memory budget the caller
    /// configured, so it is reported as an invalid argument.
    Budget(String),
    Internal(String),
}

impl RenderError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArgument(_) | Self::Budget(_) => "INVALID_ARGUMENT",
            Self::Conflict(_) => "CONFLICT",
            Self::CorruptData(_) => "CORRUPT_DATA",
            Self::GpuLost(_) => "GPU_LOST",
            Self::GpuUnsupported(_) => "GPU_UNSUPPORTED",
            Self::Internal(_) => "INTERNAL",
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Self::InvalidArgument(d)
            | Self::Conflict(d)
            | Self::CorruptData(d)
            | Self::GpuLost(d)
            | Self::GpuUnsupported(d)
            | Self::Budget(d)
            | Self::Internal(d) => d,
        }
    }
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for RenderError {}

impl From<TileError> for RenderError {
    fn from(error: TileError) -> Self {
        match error {
            TileError::Corrupt(hash) => Self::CorruptData(format!("corrupt object {hash}")),
            TileError::WrongLength { expected, got } => {
                Self::CorruptData(format!("tile length {got}, expected {expected}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::ContentHash;

    #[test]
    fn display_carries_the_sdk_code() {
        assert_eq!(
            RenderError::GpuLost("surface".into()).to_string(),
            "GPU_LOST: surface"
        );
        assert_eq!(
            RenderError::Budget("GPU budget: visible tile".into()).to_string(),
            "INVALID_ARGUMENT: GPU budget: visible tile"
        );
        let hash = ContentHash::of(b"tile");
        assert_eq!(
            RenderError::from(TileError::Corrupt(hash)),
            RenderError::CorruptData(format!("corrupt object {hash}"))
        );
    }
}
