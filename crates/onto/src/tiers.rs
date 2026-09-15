//! Agent capability tiers T1–T4 (Zhang 2026, Ch. 11).
//!
//! A lower tier cannot see or call a higher syscall. Illegal ranks do not
//! exist as variants, so they do not compile. T4 auto runs only inside a
//! declared bound `{action_type × object_set × risk_band}`; this unit starts
//! with an empty bound (no auto).
//!
//! # Context
//! Consumer syscalls are gated here. Engine and surface call
//! [`require_syscall`] / [`require_auto`]; they do not re-encode the matrix.
//!
//! # Inputs / outputs
//! [`AgentTier`] is the only actor rank. [`min_tier_for_syscall`] maps a tool
//! name (including `action.*` → submit) to the minimum tier that may see or
//! call it. [`AutoBound::empty`] denies every auto claim.
//!
//! # Side effects
//! None. Pure predicates plus [`OntoError::Denied`].
//!
//! # Relations
//! [`crate::types::Actor::tier`], [`crate::Engine::list_tools`],
//! [`crate::surface::dispatch`], confirm/override on [`crate::Engine`].

use crate::error::{OntoError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Capability rank. There is no T0 or T5 variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum AgentTier {
    /// Observe: search, get, traverse, aggregate, missing evidence.
    T1,
    /// Propose: `describe_action`, `submit_action`.
    T2,
    /// Confirm: inbox, confirm, override. Confirmer ≠ proposer.
    T3,
    /// Auto only inside a declared [`AutoBound`].
    T4,
}

impl AgentTier {
    pub const ALL: [AgentTier; 4] = [Self::T1, Self::T2, Self::T3, Self::T4];

    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::T1 => 1,
            Self::T2 => 2,
            Self::T3 => 3,
            Self::T4 => 4,
        }
    }

    #[must_use]
    pub fn allows_syscall(self, tool: &str) -> bool {
        self >= min_tier_for_syscall(tool)
    }
}

impl fmt::Display for AgentTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
            Self::T4 => "T4",
        })
    }
}

impl From<AgentTier> for u8 {
    fn from(tier: AgentTier) -> u8 {
        tier.rank()
    }
}

impl TryFrom<u8> for AgentTier {
    type Error = IllegalTier;

    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::T1),
            2 => Ok(Self::T2),
            3 => Ok(Self::T3),
            4 => Ok(Self::T4),
            other => Err(IllegalTier(other)),
        }
    }
}

/// Runtime parse failure. Not a fifth variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTier(pub u8);

impl fmt::Display for IllegalTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "illegal agent tier {}", self.0)
    }
}

impl std::error::Error for IllegalTier {}

/// Risk axis of a T4 auto claim (Zhang 2026, Ch. 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskBand {
    Low,
    Medium,
    High,
}

impl RiskBand {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(OntoError::Invalid(format!("unknown risk_band {other}"))),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// One cell of `{action_type × object_set × risk_band}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoClaim {
    pub action_type: String,
    pub object_set: String,
    pub risk_band: RiskBand,
}

/// Declared T4 auto envelope. Empty means no auto.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoBound {
    entries: Vec<AutoClaim>,
}

impl AutoBound {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    #[must_use]
    pub fn allows(&self, action_type: &str, object_set: &str, risk_band: RiskBand) -> bool {
        self.entries.iter().any(|e| {
            e.action_type == action_type && e.object_set == object_set && e.risk_band == risk_band
        })
    }
}

/// Minimum tier that may list or call `tool`. `action.*` is submit.
pub fn min_tier_for_syscall(tool: &str) -> AgentTier {
    let tool = if tool.starts_with("action.") {
        "submit_action"
    } else {
        tool
    };
    match tool {
        "search_objects"
        | "get_object"
        | "traverse_links"
        | "aggregate"
        | "list_missing_evidence"
        | "list_tools"
        | "get_decision_record"
        | "get_rejection" => AgentTier::T1,
        "describe_action" | "submit_action" | "funnel_ingest" | "create_link" => AgentTier::T2,
        "list_inbox" | "confirm_action" | "override_action" => AgentTier::T3,
        _ => AgentTier::T4,
    }
}

pub fn require_syscall(tier: AgentTier, tool: &str) -> Result<()> {
    if tier.allows_syscall(tool) {
        Ok(())
    } else {
        Err(OntoError::Denied(format!(
            "syscall {tool} requires {}, actor is {tier}",
            min_tier_for_syscall(tool)
        )))
    }
}

pub fn require_distinct_confirmer(confirmer_id: &str, proposer_id: &str) -> Result<()> {
    if confirmer_id == proposer_id {
        Err(OntoError::Denied(
            "confirmer must not be the proposer".into(),
        ))
    } else {
        Ok(())
    }
}

pub fn require_auto(
    tier: AgentTier,
    bound: &AutoBound,
    action_type: &str,
    object_set: &str,
    risk_band: RiskBand,
) -> Result<()> {
    if tier != AgentTier::T4 {
        return Err(OntoError::Denied("auto requires T4".into()));
    }
    if !bound.allows(action_type, object_set, risk_band) {
        return Err(OntoError::Denied("T4 auto outside declared bound".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_rank_tiers_t1_through_t4() {
        assert_eq!(
            AgentTier::ALL,
            [AgentTier::T1, AgentTier::T2, AgentTier::T3, AgentTier::T4]
        );
        assert_eq!(AgentTier::T1.rank(), 1);
        assert_eq!(AgentTier::T4.rank(), 4);
        assert!(AgentTier::T1 < AgentTier::T2);
        assert!(AgentTier::T2 < AgentTier::T3);
        assert!(AgentTier::T3 < AgentTier::T4);
    }

    #[test]
    fn does_reject_illegal_u8_tier() {
        assert!(AgentTier::try_from(0u8).is_err());
        assert!(AgentTier::try_from(5u8).is_err());
        assert_eq!(AgentTier::try_from(1u8).unwrap(), AgentTier::T1);
        assert_eq!(u8::from(AgentTier::T3), 3);
    }

    #[test]
    fn does_hide_syscalls_above_tier() {
        assert!(!AgentTier::T1.allows_syscall("confirm_action"));
        assert!(!AgentTier::T1.allows_syscall("submit_action"));
        assert!(AgentTier::T1.allows_syscall("get_object"));
        assert!(AgentTier::T1.allows_syscall("search_objects"));
        assert!(AgentTier::T2.allows_syscall("submit_action"));
        assert!(AgentTier::T2.allows_syscall("describe_action"));
        assert!(!AgentTier::T2.allows_syscall("confirm_action"));
        assert!(!AgentTier::T2.allows_syscall("override_action"));
        assert!(AgentTier::T3.allows_syscall("confirm_action"));
        assert!(AgentTier::T3.allows_syscall("override_action"));
        assert!(!AgentTier::T3.allows_syscall("auto_action"));
        assert!(require_syscall(AgentTier::T1, "confirm_action").is_err());
        assert!(require_syscall(AgentTier::T2, "override_action").is_err());
    }

    #[test]
    fn does_treat_action_prefix_as_submit() {
        assert_eq!(
            min_tier_for_syscall("action.propose_setpoint_change"),
            AgentTier::T2
        );
        assert!(!AgentTier::T1.allows_syscall("action.propose_setpoint_change"));
    }

    #[test]
    fn does_deny_t4_auto_if_bound_is_empty() {
        let bound = AutoBound::empty();
        assert!(!bound.allows(
            "request_sensor_calibration",
            "aeration_tanks",
            RiskBand::Low
        ));
        assert!(require_auto(
            AgentTier::T4,
            &bound,
            "request_sensor_calibration",
            "aeration_tanks",
            RiskBand::Low
        )
        .is_err());
        assert!(require_auto(
            AgentTier::T3,
            &bound,
            "request_sensor_calibration",
            "aeration_tanks",
            RiskBand::Low
        )
        .is_err());
    }

    #[test]
    fn does_allow_t4_auto_if_claim_matches() {
        let bound = AutoBound {
            entries: vec![AutoClaim {
                action_type: "request_sensor_calibration".into(),
                object_set: "aeration_tanks".into(),
                risk_band: RiskBand::Low,
            }],
        };
        assert!(bound.allows(
            "request_sensor_calibration",
            "aeration_tanks",
            RiskBand::Low
        ));
        assert!(!bound.allows(
            "request_sensor_calibration",
            "aeration_tanks",
            RiskBand::High
        ));
        assert!(!bound.allows("override_setpoint", "aeration_tanks", RiskBand::Low));
        require_auto(
            AgentTier::T4,
            &bound,
            "request_sensor_calibration",
            "aeration_tanks",
            RiskBand::Low,
        )
        .unwrap();
    }

    #[test]
    fn does_refuse_confirm_if_confirmer_is_proposer() {
        require_distinct_confirmer("ops.chen", "ops.maya").unwrap();
        assert!(require_distinct_confirmer("ops.chen", "ops.chen").is_err());
    }

    #[test]
    fn does_serde_tier_as_rank_u8() {
        let raw = serde_json::to_string(&AgentTier::T2).unwrap();
        assert_eq!(raw, "2");
        let back: AgentTier = serde_json::from_str("3").unwrap();
        assert_eq!(back, AgentTier::T3);
        assert!(serde_json::from_str::<AgentTier>("5").is_err());
    }
}
