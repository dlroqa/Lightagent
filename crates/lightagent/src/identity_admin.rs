//! `lightagent identity`: local, operator-only management of the durable
//! identity store (users, credentials, ownership, grants and audit).
//!
//! Every command acts as the local operator: the OS account that owns the
//! Lightagent home (decision C4). Nothing here is reachable over the network,
//! and nothing activates multi-user mode: `lightagent serve` does not read
//! this store. Read-only commands never create it; it is created by the
//! first change an operator makes.
//!
//! A credential's secret exists once, when it is issued or rotated. It is
//! printed only to an interactive terminal, outside CI, and never inside
//! `--json` output; otherwise it must go to a new owner-only file named
//! with `--secret-file`. If it cannot be delivered, the credential is revoked
//! before the command returns.

use std::collections::BTreeSet;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use lightagent_core::identity::store::{
    IdentityDocument, IdentityStore, Outcome, PrincipalStatus, SCHEMA_VERSION, is_denial,
};
use lightagent_core::identity::{
    Assignment, Caller, Capability, IssuedToken, PrincipalId, PrincipalSource, Restriction, Role,
};
use lightagent_core::{LightagentPaths, ProfileId};
use serde_json::{Value, json};

/// What the operator asks of the identity store.
#[derive(Clone, Debug, Subcommand)]
pub(crate) enum IdentityAction {
    /// Report the store: schema, generation and counts. No secrets.
    Status,
    /// Create, list or suspend ordinary user principals.
    User {
        #[command(subcommand)]
        action: UserAction,
    },
    /// Issue, rotate, revoke or list user credentials.
    Credential {
        #[command(subcommand)]
        action: CredentialAction,
    },
    /// Assign, reassign or remove a profile's owner.
    Owner {
        #[command(subcommand)]
        action: OwnerAction,
    },
    /// List profile ownership and grants, or revoke a grant.
    Grants {
        #[command(subcommand)]
        action: Option<GrantsAction>,
    },
    /// Show the most recent audit records: codes and identifiers only.
    Audit {
        /// How many records to show, newest last.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum UserAction {
    /// Create an ordinary user principal. Its id is permanent.
    Create { id: String },
    /// List principals with their status and credential counts.
    List,
    /// Suspend a user: every credential it holds is revoked, and any run it
    /// started stops at its next check. Reactivating does not restore them.
    Suspend { id: String },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum CredentialAction {
    /// Issue a credential for a user, optionally narrowed and expiring.
    Issue {
        /// The user principal.
        principal: String,
        /// Expire this many seconds from now.
        #[arg(long)]
        expires_in: Option<u64>,
        /// Allow only this capability (repeat for more). Default: whatever
        /// the user's ownership allows.
        #[arg(long = "capability", value_parser = parse_capability)]
        capabilities: Vec<Capability>,
        /// Allow only this profile (repeat for more).
        #[arg(long = "profile")]
        profiles: Vec<String>,
        /// Write the secret to this new file (created owner-only) instead of
        /// the terminal.
        #[arg(long)]
        secret_file: Option<PathBuf>,
    },
    /// Issue a replacement with the same principal and restriction. The old
    /// credential keeps working until it is revoked.
    Rotate {
        key_id: String,
        #[arg(long)]
        expires_in: Option<u64>,
        #[arg(long)]
        secret_file: Option<PathBuf>,
    },
    /// Revoke a credential. Runs it started stop at their next check.
    Revoke { key_id: String },
    /// List credentials by key id. Never a secret or verifier.
    List {
        #[arg(long)]
        principal: Option<String>,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum OwnerAction {
    /// Make a user the owner of an existing, unowned profile.
    Assign { profile: String, principal: String },
    /// Move a profile to another user; the previous owner's runs on it stop.
    Reassign { profile: String, principal: String },
    /// Remove a profile's owner; it becomes operator-only again.
    Remove { profile: String },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum GrantsAction {
    /// List owners and grants (the default).
    List {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Revoke one capability a principal was granted on a profile.
    Revoke {
        profile: String,
        principal: String,
        #[arg(value_parser = parse_capability)]
        capability: Capability,
    },
}

fn parse_capability(value: &str) -> Result<Capability, String> {
    Capability::ALL
        .into_iter()
        .find(|capability| capability.as_str() == value)
        .ok_or_else(|| {
            let known: Vec<&str> = Capability::ALL.iter().map(|c| c.as_str()).collect();
            format!("unknown capability (expected one of: {})", known.join(", "))
        })
}

/// The result of one command: a report that never holds a secret, and the
/// secret to deliver when one was issued.
pub(crate) struct Executed {
    pub(crate) report: Value,
    pub(crate) secret: Option<IssuedToken>,
}

impl Executed {
    fn of(report: Value) -> Self {
        Self {
            report,
            secret: None,
        }
    }
}

/// The local operator: the OS account that owns this home.
fn operator() -> Caller {
    Caller::local_operator(PrincipalSource::LocalProcess)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn principal_id(raw: &str) -> Result<PrincipalId, String> {
    PrincipalId::new(raw).map_err(|_| "invalid principal id".to_owned())
}

fn profile_id(raw: &str) -> Result<ProfileId, String> {
    ProfileId::new(raw).map_err(|_| "invalid profile id".to_owned())
}

fn expiry(now: u64, expires_in: Option<u64>) -> Result<Option<u64>, String> {
    match expires_in {
        None => Ok(None),
        Some(0) => Err("--expires-in must be at least one second".to_owned()),
        Some(seconds) => now
            .checked_add(seconds)
            .map(Some)
            .ok_or_else(|| "--expires-in is too large".to_owned()),
    }
}

fn load(store: &IdentityStore) -> Result<IdentityDocument, String> {
    store.load().map_err(|error| error.to_string())
}

fn generation<R>(outcome: &Outcome<R>) -> u64 {
    match outcome {
        Outcome::Applied { generation, .. } | Outcome::Replayed { generation } => *generation,
    }
}

/// Run one command against `store` as `actor` at `now`. The report never
/// holds a secret.
pub(crate) fn execute(
    store: &IdentityStore,
    actor: &Caller,
    now: u64,
    action: &IdentityAction,
) -> Result<Executed, String> {
    let created = !store.exists();
    let changed = |outcome_generation: u64, mut report: Value| {
        report["generation"] = json!(outcome_generation);
        report["store_created"] = json!(created);
        report
    };
    match action {
        IdentityAction::Status => status(store).map(Executed::of),
        IdentityAction::User { action } => match action {
            UserAction::Create { id } => {
                let id = principal_id(id)?;
                let outcome = store
                    .transact(actor, now, None, |tx| {
                        tx.create_principal(id.clone(), Role::User, None)
                    })
                    .map_err(|error| error.to_string())?;
                Ok(Executed::of(changed(
                    generation(&outcome),
                    json!({"created": id.as_str(), "role": "user"}),
                )))
            }
            UserAction::List => principals(store).map(Executed::of),
            UserAction::Suspend { id } => {
                let id = principal_id(id)?;
                user_role(store, &id)?;
                let outcome = store
                    .transact(actor, now, None, |tx| {
                        tx.set_status(&id, PrincipalStatus::Suspended)
                    })
                    .map_err(|error| error.to_string())?;
                Ok(Executed::of(changed(
                    generation(&outcome),
                    json!({"suspended": id.as_str(), "credentials_revoked": true}),
                )))
            }
        },
        IdentityAction::Credential { action } => match action {
            CredentialAction::Issue {
                principal,
                expires_in,
                capabilities,
                profiles,
                secret_file: _,
            } => {
                let principal = principal_id(principal)?;
                user_role(store, &principal)?;
                let restriction = Restriction {
                    capabilities: (!capabilities.is_empty())
                        .then(|| capabilities.iter().copied().collect::<BTreeSet<_>>()),
                    profiles: if profiles.is_empty() {
                        None
                    } else {
                        Some(
                            profiles
                                .iter()
                                .map(String::as_str)
                                .map(profile_id)
                                .collect::<Result<BTreeSet<_>, _>>()?,
                        )
                    },
                };
                let expires = expiry(now, *expires_in)?;
                let outcome = store
                    .transact(actor, now, None, |tx| {
                        tx.issue_credential(&principal, restriction.clone(), expires)
                    })
                    .map_err(|error| error.to_string())?;
                let generation = generation(&outcome);
                let Outcome::Applied {
                    value: (key_id, token),
                    ..
                } = outcome
                else {
                    return Err("the credential was not issued".to_owned());
                };
                Ok(Executed {
                    report: changed(
                        generation,
                        json!({
                            "issued": key_id,
                            "principal": principal.as_str(),
                            "expires_unix": expires,
                            "restriction": restriction_report(&restriction),
                        }),
                    ),
                    secret: Some(token),
                })
            }
            CredentialAction::Rotate {
                key_id,
                expires_in,
                secret_file: _,
            } => {
                let document = load(store)?;
                let Some(record) = document.credentials().get(key_id) else {
                    return Err("refused: unknown_credential".to_owned());
                };
                if record.role != Role::User {
                    return Err("only user credentials are managed here".to_owned());
                }
                let expires = expiry(now, *expires_in)?;
                let outcome = store
                    .transact(actor, now, None, |tx| tx.rotate_credential(key_id, expires))
                    .map_err(|error| error.to_string())?;
                let generation = generation(&outcome);
                let Outcome::Applied {
                    value: (new_id, token),
                    ..
                } = outcome
                else {
                    return Err("the credential was not rotated".to_owned());
                };
                Ok(Executed {
                    report: changed(
                        generation,
                        json!({
                            "issued": new_id,
                            "replaces": key_id,
                            "expires_unix": expires,
                            "previous_still_valid": true,
                        }),
                    ),
                    secret: Some(token),
                })
            }
            CredentialAction::Revoke { key_id } => {
                let outcome = store
                    .transact(actor, now, None, |tx| tx.revoke_credential(key_id))
                    .map_err(|error| error.to_string())?;
                Ok(Executed::of(changed(
                    generation(&outcome),
                    json!({"revoked": key_id}),
                )))
            }
            CredentialAction::List { principal } => {
                let principal = principal.as_deref().map(principal_id).transpose()?;
                credentials(store, principal.as_ref(), now).map(Executed::of)
            }
        },
        IdentityAction::Owner { action } => {
            let (profile, report) = match action {
                OwnerAction::Assign { profile, principal } => {
                    let (profile, principal) = (profile_id(profile)?, principal_id(principal)?);
                    let outcome = store
                        .transact(actor, now, None, |tx| tx.assign_owner(&profile, &principal))
                        .map_err(|error| error.to_string())?;
                    let assignment = match &outcome {
                        Outcome::Applied { value, .. } => assignment_code(*value),
                        Outcome::Replayed { .. } => "unchanged",
                    };
                    (
                        profile.clone(),
                        changed(
                            generation(&outcome),
                            json!({"owner": principal.as_str(), "assignment": assignment}),
                        ),
                    )
                }
                OwnerAction::Reassign { profile, principal } => {
                    let (profile, principal) = (profile_id(profile)?, principal_id(principal)?);
                    let outcome = store
                        .transact(actor, now, None, |tx| {
                            tx.reassign_owner(&profile, &principal)
                        })
                        .map_err(|error| error.to_string())?;
                    let assignment = match &outcome {
                        Outcome::Applied { value, .. } => assignment_code(*value),
                        Outcome::Replayed { .. } => "unchanged",
                    };
                    (
                        profile.clone(),
                        changed(
                            generation(&outcome),
                            json!({"owner": principal.as_str(), "assignment": assignment}),
                        ),
                    )
                }
                OwnerAction::Remove { profile } => {
                    let profile = profile_id(profile)?;
                    let outcome = store
                        .transact(actor, now, None, |tx| tx.remove_profile(&profile))
                        .map_err(|error| error.to_string())?;
                    (
                        profile.clone(),
                        changed(generation(&outcome), json!({"owner": null})),
                    )
                }
            };
            let mut report = report;
            report["profile"] = json!(profile.as_str());
            Ok(Executed::of(report))
        }
        IdentityAction::Grants { action } => match action {
            None => grants(store, None).map(Executed::of),
            Some(GrantsAction::List { profile }) => {
                let profile = profile.as_deref().map(profile_id).transpose()?;
                grants(store, profile.as_ref()).map(Executed::of)
            }
            Some(GrantsAction::Revoke {
                profile,
                principal,
                capability,
            }) => {
                let (profile, principal) = (profile_id(profile)?, principal_id(principal)?);
                let outcome = store
                    .transact(actor, now, None, |tx| {
                        tx.revoke_grant(&profile, &principal, *capability)
                    })
                    .map_err(|error| error.to_string())?;
                Ok(Executed::of(changed(
                    generation(&outcome),
                    json!({
                        "profile": profile.as_str(),
                        "principal": principal.as_str(),
                        "revoked": capability.as_str(),
                    }),
                )))
            }
        },
        IdentityAction::Audit { limit } => audit(store, *limit).map(Executed::of),
    }
}

/// Credentials are managed here for ordinary users only.
fn user_role(store: &IdentityStore, id: &PrincipalId) -> Result<(), String> {
    let document = load(store)?;
    match document.principal(id).map(|record| record.role) {
        Some(Role::User) => Ok(()),
        Some(Role::Operator) => Err("only user principals are managed here".to_owned()),
        None => Err("refused: unknown_principal".to_owned()),
    }
}

fn assignment_code(assignment: Assignment) -> &'static str {
    match assignment {
        Assignment::Created => "created",
        Assignment::Unchanged => "unchanged",
        Assignment::Reassigned => "reassigned",
    }
}

fn restriction_report(restriction: &Restriction) -> Value {
    json!({
        "capabilities": restriction
            .capabilities
            .as_ref()
            .map(|set| set.iter().map(|c| c.as_str()).collect::<Vec<_>>()),
        "profiles": restriction
            .profiles
            .as_ref()
            .map(|set| set.iter().map(|p| p.as_str()).collect::<Vec<_>>()),
    })
}

fn status(store: &IdentityStore) -> Result<Value, String> {
    if !store.exists() {
        return Ok(json!({
            "store": "absent",
            "schema_supported": SCHEMA_VERSION,
            "multi_user": "not_activated",
        }));
    }
    let document = load(store)?;
    let now = now_unix();
    let (mut users, mut operators, mut suspended) = (0, 0, 0);
    for record in document.principals() {
        match record.role {
            Role::User => users += 1,
            Role::Operator => operators += 1,
        }
        if record.status == PrincipalStatus::Suspended {
            suspended += 1;
        }
    }
    let (mut active, mut revoked, mut expired) = (0, 0, 0);
    for record in document.credentials().records() {
        match credential_state(record.revoked_unix, record.expires_unix, now) {
            "revoked" => revoked += 1,
            "expired" => expired += 1,
            _ => active += 1,
        }
    }
    let recorded = document.recorded_ownership().profiles().count();
    let effective = store.effective_ownership(&document).profiles().count();
    Ok(json!({
        "store": "present",
        "schema_supported": SCHEMA_VERSION,
        "generation": document.generation(),
        "principals": {"users": users, "operators": operators, "suspended": suspended},
        "credentials": {"active": active, "revoked": revoked, "expired": expired},
        "owned_profiles": {"recorded": recorded, "effective": effective},
        "multi_user": "not_activated",
    }))
}

fn credential_state(revoked: Option<u64>, expires: Option<u64>, now: u64) -> &'static str {
    if revoked.is_some() {
        "revoked"
    } else if expires.is_some_and(|at| now >= at) {
        "expired"
    } else {
        "active"
    }
}

fn principals(store: &IdentityStore) -> Result<Value, String> {
    let document = load(store)?;
    let rows: Vec<Value> = document
        .principals()
        .map(|record| {
            let held = document
                .credentials()
                .records()
                .filter(|credential| credential.principal == record.id);
            let active = held
                .filter(|credential| credential.revoked_unix.is_none())
                .count();
            json!({
                "id": record.id.as_str(),
                "role": record.role,
                "status": record.status,
                "created_unix": record.created_unix,
                "updated_unix": record.updated_unix,
                "unrevoked_credentials": active,
            })
        })
        .collect();
    Ok(json!({"generation": document.generation(), "principals": rows}))
}

fn credentials(
    store: &IdentityStore,
    principal: Option<&PrincipalId>,
    now: u64,
) -> Result<Value, String> {
    let document = load(store)?;
    let rows: Vec<Value> = document
        .credentials()
        .records()
        .filter(|record| principal.is_none_or(|id| &record.principal == id))
        .map(|record| {
            json!({
                "key_id": record.key_id,
                "principal": record.principal.as_str(),
                "role": record.role,
                "state": credential_state(record.revoked_unix, record.expires_unix, now),
                "created_unix": record.created_unix,
                "expires_unix": record.expires_unix,
                "revoked_unix": record.revoked_unix,
                "replaces": record.replaces,
                "restriction": restriction_report(&record.restriction),
            })
        })
        .collect();
    Ok(json!({"generation": document.generation(), "credentials": rows}))
}

fn grants(store: &IdentityStore, only: Option<&ProfileId>) -> Result<Value, String> {
    let document = load(store)?;
    let effective = store.effective_ownership(&document);
    let rows: Vec<Value> = document
        .recorded_ownership()
        .profiles()
        .filter(|(profile, _)| only.is_none_or(|id| id == *profile))
        .map(|(profile, ownership)| {
            let grants: Vec<Value> = ownership
                .grants
                .iter()
                .map(|(principal, capabilities)| {
                    json!({
                        "principal": principal.as_str(),
                        "capabilities": capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "profile": profile.as_str(),
                "owner": ownership.owner.as_str(),
                // Ownership of a deleted or replaced profile does not count.
                "effective": effective.get(profile).is_some(),
                "grants": grants,
            })
        })
        .collect();
    Ok(json!({"generation": document.generation(), "profiles": rows}))
}

fn audit(store: &IdentityStore, limit: usize) -> Result<Value, String> {
    let records = store.audit().map_err(|error| error.to_string())?;
    let skip = records.len().saturating_sub(limit);
    let rows: Vec<Value> = records
        .iter()
        .skip(skip)
        .map(|record| {
            json!({
                "at_unix": record.record.at_unix,
                "generation": record.generation,
                "action": record.record.action,
                "principal": record.record.principal,
                "key_id": record.record.key_id,
                "subject": record.subject,
                "profile": record.record.profile,
                "outcome": record.record.outcome,
                "denied": is_denial(record),
            })
        })
        .collect();
    Ok(json!({"records": rows}))
}

/// Where a new secret goes. Chosen, and the file created, before anything
/// is issued.
enum Delivery {
    Terminal,
    File(std::fs::File, PathBuf),
}

fn in_ci() -> bool {
    ["CI", "GITHUB_ACTIONS"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn delivery(secret_file: Option<&Path>, json: bool) -> Result<Delivery, String> {
    if let Some(path) = secret_file {
        return create_secret_file(path).map(|file| Delivery::File(file, path.to_path_buf()));
    }
    if json {
        return Err(
            "--json output never carries a credential; pass --secret-file <new file>".to_owned(),
        );
    }
    if in_ci() {
        return Err(
            "refusing to print a credential in CI; pass --secret-file <new file>".to_owned(),
        );
    }
    if !std::io::stdout().is_terminal() {
        return Err("standard output is not a terminal; pass --secret-file <new file>".to_owned());
    }
    Ok(Delivery::Terminal)
}

/// A new file only the owner can read. An existing file is never
/// overwritten.
fn create_secret_file(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("could not create the secret file ({})", error.kind()))
}

fn deliver(delivery: Delivery, token: &IssuedToken) -> Result<&'static str, String> {
    match delivery {
        Delivery::Terminal => {
            eprintln!("The credential follows. It is shown once and cannot be recovered:");
            println!("{}", token.expose());
            Ok("terminal")
        }
        Delivery::File(mut file, _) => {
            file.write_all(token.expose().as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .map_err(|error| format!("could not write the secret file ({})", error.kind()))?;
            Ok("file")
        }
    }
}

/// The CLI entry point.
pub(crate) fn run(action: IdentityAction, json_output: bool) -> Result<(), String> {
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    let store = IdentityStore::new(paths.root());
    let secret_file = match &action {
        IdentityAction::Credential {
            action: CredentialAction::Issue { secret_file, .. },
        }
        | IdentityAction::Credential {
            action: CredentialAction::Rotate { secret_file, .. },
        } => Some(secret_file.as_deref()),
        _ => None,
    };
    let target = match secret_file {
        Some(path) => Some(delivery(path, json_output)?),
        None => None,
    };
    let actor = operator();
    let now = now_unix();
    let executed = match execute(&store, &actor, now, &action) {
        Ok(executed) => executed,
        Err(error) => {
            // Never leave an empty secret file behind a refused command.
            if let Some(Delivery::File(file, path)) = target {
                drop(file);
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    let mut report = executed.report;
    if let (Some(token), Some(target)) = (executed.secret, target) {
        let key_id = report["issued"].as_str().unwrap_or_default().to_owned();
        match deliver(target, &token) {
            Ok(target) => report["secret_delivered_to"] = json!(target),
            Err(error) => {
                // An undelivered credential must not stay valid.
                let revoked = store
                    .transact(&actor, now, None, |tx| tx.revoke_credential(&key_id))
                    .is_ok();
                return Err(if revoked {
                    format!("{error}; the new credential {key_id} was revoked")
                } else {
                    format!("{error}; revoke the new credential {key_id} now")
                });
            }
        }
    }
    if json_output {
        println!("{report}");
    } else {
        print_report(&report);
    }
    Ok(())
}

fn print_report(report: &Value) {
    match report {
        Value::Object(map) => {
            for (key, value) in map {
                match value {
                    Value::Array(rows) => {
                        println!("{key}:");
                        for row in rows {
                            println!("  {row}");
                        }
                    }
                    Value::String(text) => println!("{key}: {text}"),
                    other => println!("{key}: {other}"),
                }
            }
        }
        other => println!("{other}"),
    }
}

#[cfg(test)]
mod tests;
