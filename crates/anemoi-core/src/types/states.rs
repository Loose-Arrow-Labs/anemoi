use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidencyState {
    Cold,
    Loading,
    WarmCpu,
    Partial,
    HotGpu,
    Serving,
    Draining,
    Evicting,
    Failed,
}

impl ResidencyState {
    pub fn is_resident(&self) -> bool {
        !matches!(self, Self::Cold | Self::Failed)
    }

    pub fn reuse_bonus(&self) -> i32 {
        match self {
            Self::HotGpu | Self::Serving => 60,
            Self::WarmCpu => 35,
            Self::Partial | Self::Loading => 15,
            Self::Draining | Self::Evicting => -10,
            Self::Cold | Self::Failed => 0,
        }
    }
}
