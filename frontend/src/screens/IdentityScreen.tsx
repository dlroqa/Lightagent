import { useEffect, useState } from "react";

import {
  agentApi,
  type IdentityAuditRecord,
  type IdentityCredential,
  type IdentityOwnership,
  type IdentityPrincipal,
  type IdentityStatus,
  type ProfileSummary,
} from "../api/agent";
import { Empty } from "../components/Bits";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";

/** What a user credential may be narrowed to; administer and provision stay the operator's. */
const USER_CAPABILITIES = ["runs_read", "runs_write", "sessions_read", "sessions_write", "tools_read", "approvals_write"];
const ALL_CAPABILITIES = [...USER_CAPABILITIES, "administer", "provision"];

const errorStyle = { fontSize: 11.5, color: "var(--danger)", marginTop: 4 } as const;
const rowStyle = { display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" } as const;
const listStyle = { margin: 0, paddingLeft: 0, listStyle: "none", display: "grid", gap: 10 } as const;

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

function when(unix: number | null | undefined): string {
  return unix ? new Date(unix * 1000).toLocaleString() : "never";
}

function shown(value: unknown): string {
  if (value === null || value === undefined) return "—";
  return typeof value === "string" ? value : JSON.stringify(value);
}

/** A whole number of seconds, or nothing. */
function seconds(raw: string): number | undefined {
  const value = Number(raw);
  return raw.trim() && Number.isInteger(value) && value > 0 ? value : undefined;
}

/**
 * The home's identity store, through the operations `lightagent identity`
 * shares, as the local operator: status, users, credentials, ownership and
 * grants, and the audit. A credential's secret is written by the runtime
 * to a new file the operator names, once; it never reaches this screen.
 */
export function IdentityScreen() {
  const [status, setStatus] = useState<IdentityStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [users, setUsers] = useState<IdentityPrincipal[]>([]);
  const [credentials, setCredentials] = useState<IdentityCredential[]>([]);
  const [owners, setOwners] = useState<IdentityOwnership[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [audit, setAudit] = useState<IdentityAuditRecord[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<{ area: string; text: string } | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);

  const [newUser, setNewUser] = useState("");
  const [issuePrincipal, setIssuePrincipal] = useState("");
  const [issueExpires, setIssueExpires] = useState("");
  const [issueCapabilities, setIssueCapabilities] = useState<string[]>([]);
  const [issueProfiles, setIssueProfiles] = useState("");
  const [issueFile, setIssueFile] = useState("");
  const [principalFilter, setPrincipalFilter] = useState("");
  const [rotating, setRotating] = useState<string | null>(null);
  const [rotateFile, setRotateFile] = useState("");
  const [rotateExpires, setRotateExpires] = useState("");
  const [ownerProfile, setOwnerProfile] = useState("");
  const [ownerPrincipal, setOwnerPrincipal] = useState("");
  const [reassignTo, setReassignTo] = useState<Record<string, string>>({});
  const [grantsProfile, setGrantsProfile] = useState("");
  const [grantProfile, setGrantProfile] = useState("");
  const [grantPrincipal, setGrantPrincipal] = useState("");
  const [grantCapability, setGrantCapability] = useState("runs_read");
  const [auditLimit, setAuditLimit] = useState("50");

  async function load(filter = principalFilter, profile = grantsProfile) {
    try {
      const [nextStatus, nextUsers, nextCredentials, nextOwners, catalog] = await Promise.all([
        agentApi.identityStatus(),
        agentApi.identityUsers(),
        agentApi.identityCredentials(filter.trim() || undefined),
        agentApi.identityGrants(profile || undefined),
        agentApi.profiles(),
      ]);
      setStatus(nextStatus);
      setUsers(nextUsers.principals);
      setCredentials(nextCredentials.credentials);
      setOwners(nextOwners.profiles);
      setProfiles(catalog.profiles);
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  /** Run one change; its outcome is announced, and a refusal is shown in `area`. */
  async function act(area: string, work: () => Promise<string>) {
    setBusy(true);
    setError(null);
    setMessage(null);
    setConfirm(null);
    try {
      setMessage(await work());
    } catch (cause) {
      setError({ area, text: errorText(cause) });
    } finally {
      setBusy(false);
      void load();
    }
  }

  const refusal = (area: string) =>
    error?.area === area ? <div role="alert" data-testid="identity-error" style={errorStyle}>{error.text}</div> : null;

  /** A destructive action that asks first: Confirm or Keep it. */
  const confirmable = (id: string, area: string, label: string, confirmLabel: string, run: () => Promise<string>, disabled = false) =>
    confirm === id ? (
      <>
        <button type="button" className="btn btn--danger" disabled={busy} onClick={() => void act(area, run)}>{confirmLabel}</button>
        <button type="button" className="btn" disabled={busy} onClick={() => setConfirm(null)}>Keep it</button>
      </>
    ) : (
      <button type="button" className="btn" disabled={busy || disabled} onClick={() => setConfirm(id)}>{label}</button>
    );

  const activeUsers = users.filter((user) => user.role === "user" && user.status === "active");
  const userOptions = (exclude?: string) => [
    <option key="" value="">Choose a user</option>,
    ...activeUsers
      .filter((user) => user.id !== exclude)
      .map((user) => <option key={user.id} value={user.id}>{user.id}</option>),
  ];
  const profileOptions = (none: string) => [
    <option key="" value="">{none}</option>,
    ...profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name || profile.id}</option>),
  ];

  return (
    <>
      <TopBar title="Identity" subtitle="Users, credentials and profile ownership for this home (operator only)" />
      <div className="page" data-testid="identity-screen">
        {message && <div className="notice notice--info" role="status" data-testid="identity-message">{message}</div>}
        {loadError && (
          <div className="notice notice--warn" role="alert">
            The identity store is unavailable: {loadError}{" "}
            <button type="button" className="btn" onClick={() => void load()}>Retry</button>
          </div>
        )}

        <Card title="Identity store">
          <p className="card__note">
            Shared with the terminal CLI (lightagent identity). Only the local operator manages identity: the desktop app,
            or this Web UI served on loopback without a key.
          </p>
          {!status && !loadError && <span className="muted">Loading…</span>}
          {status && (
            <div data-testid="identity-status">
              {status.store === "absent" ? (
                <div>No identity store yet. It is created by the first change below.</div>
              ) : (
                <ul style={{ margin: 0, paddingLeft: 18 }}>
                  <li>Generation {status.generation}</li>
                  <li>
                    {status.principals?.users ?? 0} users, {status.principals?.operators ?? 0} operators,{" "}
                    {status.principals?.suspended ?? 0} suspended
                  </li>
                  <li>
                    Credentials: {status.credentials?.active ?? 0} active, {status.credentials?.revoked ?? 0} revoked,{" "}
                    {status.credentials?.expired ?? 0} expired
                  </li>
                  <li>Owned profiles: {status.owned_profiles?.effective ?? 0} in effect of {status.owned_profiles?.recorded ?? 0} recorded</li>
                </ul>
              )}
              {status.multi_user === "not_activated" && (
                <div className="muted" style={{ marginTop: 6 }}>
                  Multi-user mode is not active: lightagent serve does not read this store, so these records change no
                  one&apos;s access yet.
                </div>
              )}
            </div>
          )}
        </Card>

        <Card title="Users">
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field" style={{ flex: "1 1 220px" }}>
              <label className="field__label" htmlFor="identity-user-id">New user id (permanent)</label>
              <input id="identity-user-id" className="input" value={newUser} spellCheck={false} disabled={busy}
                onChange={(event) => setNewUser(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={busy || !newUser.trim()}
              onClick={() => void act("users", async () => {
                const created = await agentApi.createIdentityUser(newUser.trim());
                setNewUser("");
                return `Created user ${created.created}.`;
              })}>Create user</button>
          </div>
          {refusal("users")}
          {users.length === 0 ? (
            <Empty title="No users" hint="Create one above." />
          ) : (
            <ul data-testid="identity-users" style={{ ...listStyle, marginTop: 12 }}>
              {users.map((user) => (
                <li key={user.id} data-testid={`user-${user.id}`} style={rowStyle}>
                  <strong>{user.id}</strong>
                  <span className="muted">
                    {user.role} · {user.status} · {user.unrevoked_credentials} unrevoked credential
                    {user.unrevoked_credentials === 1 ? "" : "s"} · since {when(user.created_unix)}
                  </span>
                  {user.role === "user" && user.status === "active" &&
                    confirmable(`suspend:${user.id}`, "users", "Suspend…", `Confirm: suspend ${user.id}`, async () => {
                      await agentApi.suspendIdentityUser(user.id);
                      return `Suspended ${user.id}; every credential it held was revoked.`;
                    })}
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card title="Issue a credential">
          <p className="card__note">
            The credential is written once to a new file only you can read, named by an absolute path; an existing file
            is never overwritten. It is not shown here and cannot be recovered.
          </p>
          <div className="field">
            <label className="field__label" htmlFor="identity-issue-principal">User</label>
            <select id="identity-issue-principal" className="input" value={issuePrincipal} disabled={busy}
              onChange={(event) => setIssuePrincipal(event.target.value)}>{userOptions()}</select>
          </div>
          <fieldset className="field" style={{ border: 0, padding: 0, margin: "8px 0" }}>
            <legend className="field__label">Allow only (optional; none means what the user&apos;s ownership allows)</legend>
            <div style={{ ...rowStyle, gap: 12 }}>
              {USER_CAPABILITIES.map((capability) => (
                <label key={capability} style={{ display: "inline-flex", gap: 4, alignItems: "center" }}>
                  <input type="checkbox" checked={issueCapabilities.includes(capability)} disabled={busy}
                    onChange={(event) => setIssueCapabilities((current) => event.target.checked
                      ? [...current, capability]
                      : current.filter((item) => item !== capability))} />
                  {capability}
                </label>
              ))}
            </div>
          </fieldset>
          <div style={rowStyle}>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label className="field__label" htmlFor="identity-issue-profiles">Only these profiles (optional, comma-separated)</label>
              <input id="identity-issue-profiles" className="input" value={issueProfiles} spellCheck={false} disabled={busy}
                onChange={(event) => setIssueProfiles(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="identity-issue-expires">Expires in seconds (optional)</label>
              <input id="identity-issue-expires" className="input" type="number" min={1} value={issueExpires} disabled={busy}
                style={{ maxWidth: 160 }} onChange={(event) => setIssueExpires(event.target.value)} />
            </div>
          </div>
          <div className="field">
            <label className="field__label" htmlFor="identity-issue-file">Write the credential to this new file (absolute path)</label>
            <input id="identity-issue-file" className="input" value={issueFile} spellCheck={false} disabled={busy}
              placeholder="/path/to/new-credential" onChange={(event) => setIssueFile(event.target.value)} />
          </div>
          {refusal("issue")}
          <button type="button" className="btn" style={{ marginTop: 10 }} aria-busy={busy || undefined}
            disabled={busy || !issuePrincipal || !issueFile.trim()}
            onClick={() => void act("issue", async () => {
              const issued = await agentApi.issueIdentityCredential({
                principal: issuePrincipal,
                expires_in: seconds(issueExpires),
                capabilities: issueCapabilities,
                profiles: issueProfiles.split(",").map((item) => item.trim()).filter(Boolean),
                secret_file: issueFile.trim(),
              });
              setIssueFile("");
              return `Issued ${issued.issued} for ${issued.principal ?? issuePrincipal}. The credential was written once to the file you named and is not shown here.`;
            })}>Issue credential</button>
        </Card>

        <Card title="Credentials">
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label className="field__label" htmlFor="identity-credential-principal">Only this user (optional)</label>
              <input id="identity-credential-principal" className="input" value={principalFilter} spellCheck={false}
                onChange={(event) => setPrincipalFilter(event.target.value)}
                onKeyDown={(event) => { if (event.key === "Enter") void load(principalFilter); }} />
            </div>
            <button type="button" className="btn" onClick={() => void load(principalFilter)}>Filter</button>
          </div>
          {refusal("credentials")}
          {credentials.length === 0 ? (
            <Empty title="No credentials" hint="Issue one above." />
          ) : (
            <ul data-testid="identity-credentials" style={{ ...listStyle, marginTop: 12 }}>
              {credentials.map((credential) => (
                <li key={credential.key_id} data-testid={`credential-${credential.key_id}`}>
                  <div style={rowStyle}>
                    <code>{credential.key_id}</code>
                    <span className="muted">
                      {credential.principal} · {credential.state} · expires {when(credential.expires_unix)}
                      {credential.replaces && ` · replaces ${credential.replaces}`}
                      {credential.restriction?.capabilities && ` · only ${credential.restriction.capabilities.join(", ")}`}
                      {credential.restriction?.profiles && ` · on ${credential.restriction.profiles.join(", ")}`}
                    </span>
                    {credential.state === "active" && (
                      <>
                        <button type="button" className="btn" disabled={busy}
                          onClick={() => { setRotating(rotating === credential.key_id ? null : credential.key_id); setRotateFile(""); setRotateExpires(""); }}>
                          Rotate…
                        </button>
                        {confirmable(`revoke:${credential.key_id}`, "credentials", "Revoke…", `Confirm: revoke ${credential.key_id}`, async () => {
                          await agentApi.revokeIdentityCredential(credential.key_id);
                          return `Revoked ${credential.key_id}.`;
                        })}
                      </>
                    )}
                  </div>
                  {rotating === credential.key_id && (
                    <div style={{ ...rowStyle, alignItems: "flex-end", marginTop: 8 }}>
                      <div className="field" style={{ flex: "1 1 240px" }}>
                        <label className="field__label" htmlFor="identity-rotate-file">Write the replacement to this new file (absolute path)</label>
                        <input id="identity-rotate-file" className="input" value={rotateFile} spellCheck={false} disabled={busy}
                          onChange={(event) => setRotateFile(event.target.value)} />
                      </div>
                      <div className="field">
                        <label className="field__label" htmlFor="identity-rotate-expires">Expires in seconds (optional)</label>
                        <input id="identity-rotate-expires" className="input" type="number" min={1} value={rotateExpires}
                          disabled={busy} style={{ maxWidth: 160 }} onChange={(event) => setRotateExpires(event.target.value)} />
                      </div>
                      <button type="button" className="btn" disabled={busy || !rotateFile.trim()}
                        onClick={() => void act("credentials", async () => {
                          const issued = await agentApi.rotateIdentityCredential(credential.key_id, {
                            expires_in: seconds(rotateExpires),
                            secret_file: rotateFile.trim(),
                          });
                          setRotating(null);
                          return `Issued ${issued.issued} to replace ${credential.key_id}; the old credential works until it is revoked.`;
                        })}>{`Rotate ${credential.key_id}`}</button>
                      <button type="button" className="btn" disabled={busy} onClick={() => setRotating(null)}>Cancel</button>
                    </div>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card title="Profile ownership">
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field">
              <label className="field__label" htmlFor="identity-owner-profile">Unowned profile</label>
              <select id="identity-owner-profile" className="input" value={ownerProfile} disabled={busy}
                onChange={(event) => setOwnerProfile(event.target.value)}>{profileOptions("Choose a profile")}</select>
            </div>
            <div className="field">
              <label className="field__label" htmlFor="identity-owner-principal">Owner</label>
              <select id="identity-owner-principal" className="input" value={ownerPrincipal} disabled={busy}
                onChange={(event) => setOwnerPrincipal(event.target.value)}>{userOptions()}</select>
            </div>
            <button type="button" className="btn" disabled={busy || !ownerProfile || !ownerPrincipal}
              onClick={() => void act("owners", async () => {
                const assigned = await agentApi.assignIdentityOwner(ownerProfile, ownerPrincipal);
                return assigned.assignment === "unchanged"
                  ? `${ownerPrincipal} already owns ${ownerProfile}.`
                  : `${ownerPrincipal} now owns ${ownerProfile}.`;
              })}>Assign owner</button>
          </div>
          <div className="field" style={{ marginTop: 12 }}>
            <label className="field__label" htmlFor="identity-grants-profile">Show</label>
            <select id="identity-grants-profile" className="input" value={grantsProfile} style={{ maxWidth: 260 }}
              onChange={(event) => { setGrantsProfile(event.target.value); void load(principalFilter, event.target.value); }}>
              {profileOptions("Every owned profile")}
            </select>
          </div>
          {refusal("owners")}
          {owners.length === 0 ? (
            <Empty title="No owned profiles" hint="Every profile is the operator's until a user is assigned." />
          ) : (
            <ul data-testid="identity-owners" style={{ ...listStyle, marginTop: 12 }}>
              {owners.map((owned) => {
                const target = reassignTo[owned.profile] ?? "";
                return (
                  <li key={owned.profile} data-testid={`owner-${owned.profile}`}>
                    <div style={rowStyle}>
                      <strong>{owned.profile}</strong>
                      <span className="muted">
                        owned by {owned.owner}
                        {!owned.effective && " (not in effect: the profile was deleted or replaced)"}
                      </span>
                    </div>
                    {owned.grants.length > 0 && (
                      <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
                        {owned.grants.map((grant) => (
                          <li key={grant.principal}>{grant.principal}: {grant.capabilities.join(", ")}</li>
                        ))}
                      </ul>
                    )}
                    <div style={{ ...rowStyle, marginTop: 6 }}>
                      <label className="muted" htmlFor={`identity-reassign-${owned.profile}`}>Reassign to</label>
                      <select id={`identity-reassign-${owned.profile}`} className="input" style={{ maxWidth: 220 }} value={target}
                        disabled={busy} onChange={(event) => setReassignTo((current) => ({ ...current, [owned.profile]: event.target.value }))}>
                        {userOptions(owned.owner)}
                      </select>
                      {confirmable(`reassign:${owned.profile}`, "owners", "Reassign…", `Confirm: reassign ${owned.profile} to ${target}`, async () => {
                        await agentApi.reassignIdentityOwner(owned.profile, target);
                        return `${owned.profile} moved to ${target}; ${owned.owner}'s runs on it stop.`;
                      }, !target)}
                      {confirmable(`remove:${owned.profile}`, "owners", "Remove owner…", `Confirm: remove the owner of ${owned.profile}`, async () => {
                        await agentApi.removeIdentityOwner(owned.profile);
                        return `${owned.profile} has no owner now; it is the operator's alone.`;
                      })}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </Card>

        <Card title="Revoke a grant">
          <p className="card__note">Removes one capability a user was granted on a profile.</p>
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field">
              <label className="field__label" htmlFor="identity-grant-profile">Profile</label>
              <select id="identity-grant-profile" className="input" value={grantProfile} disabled={busy}
                onChange={(event) => setGrantProfile(event.target.value)}>{profileOptions("Choose a profile")}</select>
            </div>
            <div className="field">
              <label className="field__label" htmlFor="identity-grant-principal">User</label>
              <input id="identity-grant-principal" className="input" value={grantPrincipal} spellCheck={false} disabled={busy}
                onChange={(event) => setGrantPrincipal(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="identity-grant-capability">Capability</label>
              <select id="identity-grant-capability" className="input" value={grantCapability} disabled={busy}
                onChange={(event) => setGrantCapability(event.target.value)}>
                {ALL_CAPABILITIES.map((capability) => <option key={capability} value={capability}>{capability}</option>)}
              </select>
            </div>
            {confirmable("grant", "grants", "Revoke grant…", `Confirm: revoke ${grantCapability}`, async () => {
              await agentApi.revokeIdentityGrant({ profile: grantProfile, principal: grantPrincipal.trim(), capability: grantCapability });
              return `Revoked ${grantCapability} for ${grantPrincipal.trim()} on ${grantProfile}.`;
            }, !grantProfile || !grantPrincipal.trim())}
          </div>
          {refusal("grants")}
        </Card>

        <Card title="Audit">
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field">
              <label className="field__label" htmlFor="identity-audit-limit">Most recent records</label>
              <input id="identity-audit-limit" className="input" type="number" min={1} value={auditLimit} style={{ maxWidth: 120 }}
                onChange={(event) => setAuditLimit(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={busy}
              onClick={() => void agentApi.identityAudit(seconds(auditLimit) ?? 50)
                .then((result) => { setAudit(result.records); setError(null); })
                .catch((cause) => setError({ area: "audit", text: errorText(cause) }))}>Show audit</button>
          </div>
          {refusal("audit")}
          {audit && (audit.length === 0 ? <span className="muted">No audit records.</span> : (
            <div style={{ overflowX: "auto", marginTop: 10 }}>
              <table data-testid="identity-audit" style={{ borderCollapse: "collapse", fontSize: 12.5, width: "100%" }}>
                <thead>
                  <tr>
                    {["When", "Action", "Principal", "Key", "Subject", "Profile", "Outcome"].map((head) => (
                      <th key={head} scope="col" style={{ textAlign: "left", padding: "4px 8px" }}>{head}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {audit.map((record, index) => (
                    <tr key={`${record.generation}-${index}`}>
                      <td style={{ padding: "4px 8px" }}>{when(record.at_unix)}</td>
                      <td style={{ padding: "4px 8px" }}>{shown(record.action)}</td>
                      <td style={{ padding: "4px 8px" }}>{shown(record.principal)}</td>
                      <td style={{ padding: "4px 8px" }}>{shown(record.key_id)}</td>
                      <td style={{ padding: "4px 8px" }}>{shown(record.subject)}</td>
                      <td style={{ padding: "4px 8px" }}>{shown(record.profile)}</td>
                      <td style={{ padding: "4px 8px" }}>{record.denied ? `denied: ${shown(record.outcome)}` : shown(record.outcome)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ))}
        </Card>
      </div>
    </>
  );
}
