//! Authenticated local approvals. Worker tools never expose this adapter.

use crate::{
    identity::Digest, ledger::Ledger, plan::ApprovalRequirement, require, wire, Error, Result,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Authenticated OS identity, deliberately not deserializable from a request.
#[derive(Debug, Clone)]
pub struct Principal {
    actor_id: String,
}
impl Principal {
    /// Authenticate the local operator from the process's OS identity.
    pub fn current() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                actor_id: format!("uid:{}", std::fs::metadata("/proc/self")?.uid()),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(Error(
                "local harness approval authentication currently requires Linux".into(),
            ))
        }
    }
    /// Stable authenticated principal label for audit records.
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }
}

/// Trusted operator registration, stored outside candidate/workspace mounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityPolicy {
    /// Authenticated principal to allowed approval types; no wildcard authority.
    pub actors: BTreeMap<String, BTreeSet<String>>,
}
impl AuthorityPolicy {
    /// Verify exact authority; repository or model labels cannot override it.
    pub fn authorize(&self, principal: &Principal, approval_type: &str) -> Result<()> {
        require(
            self.actors
                .get(principal.actor_id())
                .is_some_and(|s| s.contains(approval_type)),
            format!("principal is not authorized for {approval_type}"),
        )
    }
}

/// Exact subject, scope, policy and lifetime of an authenticated decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    /// Immutable approval ID.
    pub approval_id: String,
    /// Optional single-run restriction; reusable contracts may be unbound.
    pub run_id: Option<String>,
    /// Exact reviewed artifact.
    pub subject_digest: Digest,
    /// Authority category.
    pub approval_type: String,
    /// OS-authenticated approver, never supplied by a model.
    pub actor_id: String,
    /// Effective policy at approval time.
    pub policy_digest: Digest,
    /// Exact authorized scope.
    pub scope_digest: Digest,
    /// UTC creation time in milliseconds.
    pub created_ms: u64,
    /// Optional exclusive expiry timestamp.
    pub expires_ms: Option<u64>,
}

/// A request for the current authenticated operator's decision; has no actor field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    /// Optional run restriction.
    pub run_id: Option<String>,
    /// Exact subject to approve.
    pub subject_digest: Digest,
    /// Required authority category.
    pub approval_type: String,
    /// Current policy identity.
    pub policy_digest: Digest,
    /// Authorized scope identity.
    pub scope_digest: Digest,
    /// Optional expiry; must be in the future.
    pub expires_ms: Option<u64>,
}

impl Ledger {
    /// Record an approval by an authenticated operator with registered authority.
    pub fn grant(
        &self,
        principal: &Principal,
        authority: &AuthorityPolicy,
        request: ApprovalRequest,
        now: u64,
    ) -> Result<Approval> {
        authority.authorize(principal, &request.approval_type)?;
        require(
            request.expires_ms.is_none_or(|until| until > now),
            "approval already expired",
        )?;
        for subject in [
            &request.subject_digest,
            &request.policy_digest,
            &request.scope_digest,
        ] {
            self.read(subject)?;
        }
        let approval = Approval {
            approval_id: uuid::Uuid::new_v4().to_string(),
            run_id: request.run_id,
            subject_digest: request.subject_digest,
            approval_type: request.approval_type,
            actor_id: principal.actor_id.clone(),
            policy_digest: request.policy_digest,
            scope_digest: request.scope_digest,
            created_ms: now,
            expires_ms: request.expires_ms,
        };
        let artifact = self.store_json("approval", &approval, now)?;
        self.connection.execute(
            "INSERT INTO approvals VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                approval.approval_id,
                approval.run_id,
                approval.subject_digest.as_str(),
                approval.approval_type,
                approval.actor_id,
                approval.policy_digest.as_str(),
                approval.scope_digest.as_str(),
                artifact.digest.as_str(),
                i64::try_from(now).map_err(|_| Error("timestamp overflow".into()))?,
                approval
                    .expires_ms
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| Error("expiry overflow".into()))?
            ],
        )?;
        Ok(approval)
    }
    /// Append a revocation; historical approvals and evidence remain immutable.
    pub fn revoke(
        &self,
        principal: &Principal,
        authority: &AuthorityPolicy,
        approval_id: &str,
        reason: &str,
        now: u64,
    ) -> Result<()> {
        authority.authorize(principal, "revoke")?;
        require(!reason.trim().is_empty(), "revocation needs a reason")?;
        let artifact=self.store_json("revocation",&serde_json::json!({"approval_id":approval_id,"actor_id":principal.actor_id(),"reason":reason,"created_ms":now}),now)?;
        self.connection.execute(
            "INSERT INTO approval_revocations VALUES(?1,?2,?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                approval_id,
                principal.actor_id(),
                artifact.digest.as_str(),
                i64::try_from(now).map_err(|_| Error("timestamp overflow".into()))?
            ],
        )?;
        Ok(())
    }
    /// Check exact subject, run, scope, policy, authority, expiry and revocations now.
    #[allow(clippy::too_many_arguments)]
    pub fn approval_current(
        &self,
        run_id: &str,
        requirement: &ApprovalRequirement,
        scope: &Digest,
        policy: &Digest,
        authority: &AuthorityPolicy,
        now: u64,
    ) -> Result<bool> {
        let mut statement=self.connection.prepare("SELECT a.approval_digest FROM approvals a WHERE a.subject_digest=?1 AND a.approval_type=?2 AND a.scope_digest=?3 AND a.policy_digest=?4 AND (a.run_id IS NULL OR a.run_id=?5) AND NOT EXISTS(SELECT 1 FROM approval_revocations r WHERE r.approval_id=a.approval_id)")?;
        for row in statement.query_map(
            params![
                requirement.subject_digest.as_str(),
                requirement.approval_type,
                scope.as_str(),
                policy.as_str(),
                run_id
            ],
            |r| r.get::<_, String>(0),
        )? {
            let approval: Approval = wire::decode(&self.read(&Digest::parse(&row?)?)?)?;
            if approval.created_ms <= now
                && approval.expires_ms.is_none_or(|until| until > now)
                && authority
                    .actors
                    .get(&approval.actor_id)
                    .is_some_and(|s| s.contains(&approval.approval_type))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
