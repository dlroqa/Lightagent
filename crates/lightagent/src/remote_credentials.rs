//! `lightagent remote-credential`: local, operator-only management of the
//! profile-scoped remote credential vault.
//!
//! Every command acts as the local operator (the OS account that owns the
//! home). A secret is read only from a file or standard input, never from
//! the command line (where it would reach shell history and the process
//! list), and is never printed. Read-only commands never create the vault.
//! The master key comes from `LIGHTAGENT_CREDENTIAL_KEY_FILE` or
//! `LIGHTAGENT_CREDENTIAL_KEY`; `keygen` writes a new key file, which must be
//! kept outside the Lightagent home.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use lightagent_core::credentials::{
    CredentialVault, ExternalSecret, GrantSpec, MasterKey, RemoteService, origin_of,
};
use lightagent_core::identity::{Caller, PrincipalSource};
use lightagent_core::{LightagentPaths, ProfileId, ProfileStore};
use serde_json::{Value, json};

/// What the operator asks of the credential vault.
#[derive(Clone, Debug, Subcommand)]
pub(crate) enum RemoteCredentialAction {
    /// Report the vault: whether it exists, its generation, grant counts and
    /// whether the configured key opens its records. No secrets.
    Status,
    /// Write a new master key to a new owner-only file outside the home.
    Keygen { path: PathBuf },
    /// Seal a secret and grant it to one profile for one service.
    Add {
        profile: String,
        /// `web-search` or `mcp:<server>`.
        #[arg(long)]
        service: String,
        /// An origin it may be sent to, `https://host[:port]` (repeat).
        #[arg(long = "destination", required = true)]
        destinations: Vec<String>,
        /// A tool or operation it may be used for (repeat). Default: every
        /// operation of the service.
        #[arg(long = "operation")]
        operations: Vec<String>,
        #[arg(long)]
        expires_in: Option<u64>,
        #[arg(long)]
        max_requests_per_run: Option<u32>,
        #[command(flatten)]
        secret: SecretInput,
    },
    /// Replace a grant's secret. Runs holding the old one stop using it.
    Rotate {
        id: String,
        #[command(flatten)]
        secret: SecretInput,
    },
    /// Revoke a grant. Runs using it stop at their next request.
    Revoke { id: String },
    /// List grants: metadata only, never a secret or ciphertext.
    List {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Show the most recent audit records.
    Audit {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Re-seal every active grant from the previous key to the configured
    /// one. Nothing changes unless every grant opens with the previous key.
    Rekey {
        #[arg(long)]
        previous_key_file: PathBuf,
    },
}

/// Where a secret is read from: a file, or standard input.
#[derive(Clone, Debug, clap::Args)]
#[group(required = true, multiple = false)]
pub(crate) struct SecretInput {
    /// Read the secret from this file (delete it afterwards).
    #[arg(long)]
    secret_file: Option<PathBuf>,
    /// Read the secret from standard input.
    #[arg(long)]
    secret_stdin: bool,
}

impl SecretInput {
    fn read(&self) -> Result<ExternalSecret, String> {
        let text = match &self.secret_file {
            Some(path) => std::fs::read_to_string(path)
                .map_err(|error| format!("could not read the secret file ({})", error.kind()))?,
            None => {
                let mut text = String::new();
                std::io::stdin()
                    .take(16 * 1024)
                    .read_to_string(&mut text)
                    .map_err(|error| format!("could not read standard input ({})", error.kind()))?;
                text
            }
        };
        ExternalSecret::new(text).map_err(|error| error.to_string())
    }
}

fn operator() -> Caller {
    Caller::local_operator(PrincipalSource::LocalProcess)
}

fn now_unix() -> u64 {
    lightagent_core::credentials::system_now()
}

fn required(key: Option<&MasterKey>) -> Result<&MasterKey, String> {
    key.ok_or_else(|| {
        "no credential key is configured: set LIGHTAGENT_CREDENTIAL_KEY_FILE (see `remote-credential keygen`)"
            .to_owned()
    })
}

/// Run one command against the vault in `home`. Reports never hold a secret.
pub(crate) fn execute(
    home: &Path,
    actor: &Caller,
    now: u64,
    key: Option<&MasterKey>,
    action: &RemoteCredentialAction,
    secret: Option<ExternalSecret>,
) -> Result<Value, String> {
    let vault = CredentialVault::new(home);
    match action {
        RemoteCredentialAction::Status => status(&vault, key),
        RemoteCredentialAction::Keygen { path } => keygen(path, home),
        RemoteCredentialAction::Add {
            profile,
            service,
            destinations,
            operations,
            expires_in,
            max_requests_per_run,
            ..
        } => {
            let key = required(key)?;
            let profile =
                ProfileId::new(profile.as_str()).map_err(|_| "invalid profile id".to_owned())?;
            if !ProfileStore::new(home)
                .handle(&profile)
                .manifest_file()
                .is_file()
            {
                return Err("refused: profile_missing".to_owned());
            }
            let service = RemoteService::parse(service)
                .ok_or_else(|| "service must be `web-search` or `mcp:<server>`".to_owned())?;
            let destinations = destinations
                .iter()
                .map(|origin| {
                    origin_of(origin)
                        .filter(|parsed| parsed == origin)
                        .ok_or_else(|| {
                            format!("destination {origin:?} must be scheme://host[:port]")
                        })
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            let expires_unix = match expires_in {
                Some(0) => return Err("--expires-in must be at least one second".to_owned()),
                Some(seconds) => Some(
                    now.checked_add(*seconds)
                        .ok_or_else(|| "--expires-in is too large".to_owned())?,
                ),
                None => None,
            };
            let secret = secret.ok_or_else(|| "a secret is required".to_owned())?;
            let id = vault
                .add(
                    actor,
                    now,
                    key,
                    GrantSpec {
                        profile: profile.clone(),
                        service: service.clone(),
                        operations: (!operations.is_empty())
                            .then(|| operations.iter().cloned().collect()),
                        destinations,
                        expires_unix,
                        max_requests_per_run: *max_requests_per_run,
                    },
                    &secret,
                )
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "added": id,
                "profile": profile.as_str(),
                "service": service.label(),
                "expires_unix": expires_unix,
                "key_fingerprint": key.fingerprint(),
            }))
        }
        RemoteCredentialAction::Rotate { id, .. } => {
            let key = required(key)?;
            let secret = secret.ok_or_else(|| "a secret is required".to_owned())?;
            let version = vault
                .rotate(actor, now, key, id, &secret)
                .map_err(|error| error.to_string())?;
            Ok(json!({"rotated": id, "version": version}))
        }
        RemoteCredentialAction::Revoke { id } => {
            vault
                .revoke(actor, now, id)
                .map_err(|error| error.to_string())?;
            Ok(json!({"revoked": id}))
        }
        RemoteCredentialAction::List { profile } => {
            let profile = profile
                .as_deref()
                .map(|raw| ProfileId::new(raw).map_err(|_| "invalid profile id".to_owned()))
                .transpose()?;
            list(&vault, profile.as_ref(), now)
        }
        RemoteCredentialAction::Audit { limit } => {
            let records = vault.audit().map_err(|error| error.to_string())?;
            let skip = records.len().saturating_sub(*limit);
            Ok(json!({"records": records.into_iter().skip(skip).collect::<Vec<_>>()}))
        }
        RemoteCredentialAction::Rekey { previous_key_file } => {
            let new = required(key)?;
            let old =
                MasterKey::from_file(previous_key_file, home).map_err(|error| error.to_string())?;
            let count = vault
                .rekey(actor, now, &old, new)
                .map_err(|error| error.to_string())?;
            Ok(json!({"resealed": count, "key_fingerprint": new.fingerprint()}))
        }
    }
}

fn status(vault: &CredentialVault, key: Option<&MasterKey>) -> Result<Value, String> {
    if !vault.exists() {
        return Ok(json!({
            "vault": "absent",
            "key_configured": key.is_some(),
        }));
    }
    let document = vault.load().map_err(|error| error.to_string())?;
    let now = now_unix();
    let (mut active, mut revoked, mut expired, mut other_key) = (0, 0, 0, 0);
    for grant in document.grants() {
        match grant.state(now) {
            Ok(()) => active += 1,
            Err(lightagent_core::credentials::CredentialDenial::Revoked) => revoked += 1,
            Err(_) => expired += 1,
        }
        if grant.revoked_unix.is_none()
            && key.is_none_or(|key| grant.sealed.key_fingerprint != key.fingerprint())
        {
            other_key += 1;
        }
    }
    Ok(json!({
        "vault": "present",
        "generation": document.generation(),
        "grants": {"active": active, "revoked": revoked, "expired": expired},
        "key_configured": key.is_some(),
        "key_fingerprint": key.map(MasterKey::fingerprint),
        "unopenable_with_configured_key": other_key,
    }))
}

fn list(vault: &CredentialVault, only: Option<&ProfileId>, now: u64) -> Result<Value, String> {
    let document = vault.load().map_err(|error| error.to_string())?;
    let rows: Vec<Value> = document
        .grants()
        .filter(|grant| only.is_none_or(|profile| &grant.profile == profile))
        .map(|grant| {
            json!({
                "id": grant.id,
                "profile": grant.profile.as_str(),
                "service": grant.service.label(),
                "operations": grant.operations,
                "destinations": grant.destinations,
                "state": match grant.state(now) {
                    Ok(()) => "active",
                    Err(denial) => denial.code(),
                },
                "version": grant.version,
                "created_unix": grant.created_unix,
                "expires_unix": grant.expires_unix,
                "revoked_unix": grant.revoked_unix,
                "rotated_unix": grant.rotated_unix,
                "max_requests_per_run": grant.max_requests_per_run,
                "key_fingerprint": grant.sealed.key_fingerprint,
            })
        })
        .collect();
    Ok(json!({"generation": document.generation(), "credentials": rows}))
}

/// A new key in a new owner-only file outside `home`.
fn keygen(path: &Path, home: &Path) -> Result<Value, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("the key directory is unavailable ({})", error.kind()))?;
    if let Ok(home) = home.canonicalize()
        && parent.starts_with(&home)
    {
        return Err("the credential key file must be outside the Lightagent home".to_owned());
    }
    let key = MasterKey::generate().map_err(|error| error.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not create the key file ({})", error.kind()))?;
    use std::io::Write as _;
    file.write_all(format!("{}\n", key.to_hex()).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write the key file ({})", error.kind()))?;
    Ok(json!({
        "key_file": "created",
        "key_fingerprint": key.fingerprint(),
        "next": "set LIGHTAGENT_CREDENTIAL_KEY_FILE to this file for the server and the CLI",
    }))
}

/// The CLI entry point.
pub(crate) fn run(action: RemoteCredentialAction, json_output: bool) -> Result<(), String> {
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    let home = paths.root();
    let key = MasterKey::from_environment(home).map_err(|error| error.to_string())?;
    let secret = match &action {
        RemoteCredentialAction::Add { secret, .. }
        | RemoteCredentialAction::Rotate { secret, .. } => Some(secret.read()?),
        _ => None,
    };
    let report = execute(home, &operator(), now_unix(), key.as_ref(), &action, secret)?;
    if json_output {
        println!("{report}");
    } else if let Value::Object(map) = &report {
        for (field, value) in map {
            match value {
                Value::Array(rows) => {
                    println!("{field}:");
                    for row in rows {
                        println!("  {row}");
                    }
                }
                Value::String(text) => println!("{field}: {text}"),
                other => println!("{field}: {other}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
