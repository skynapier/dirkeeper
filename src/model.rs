use serde::{Deserialize, Serialize};

/// Status of a planned move, serialized as a lowercase string.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MoveStatus {
    Ready,
    Conflict,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct MoveSpec {
    pub(crate) source: String,
    pub(crate) destination: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct PlanMove {
    pub(crate) source: String,
    pub(crate) destination: String,
    pub(crate) status: MoveStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reconciliation: Option<crate::task::reconcile::Reconciliation>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) version: u32,
    pub(crate) moves: Vec<PlanMove>,
}
