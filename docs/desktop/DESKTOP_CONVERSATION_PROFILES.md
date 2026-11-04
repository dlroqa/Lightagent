# Conversation profiles and the account-integration contract

Owner decision (2026-11-04): `lightagent chat --profile X` is matched by
a conversation-scoped profile, not by the profile switch. A conversation
runs as the profile it is bound to, and the active (default) profile is
never changed by choosing one. This document records the local contract
(implemented in row 57) and the contract a later online-account stage
must keep. Online login and synchronization are not part of D7-D10.

## The local contract (implemented)

- **Binding.** A conversation is bound to one agent profile when it is
  created and is kept in that profile's own store,
  `profiles/<id>/sessions`, the store `chat --profile <id>` uses. The
  binding is the `profile` field of the saved session; it never changes.
- **Naming the profile.** Every conversation operation can name its
  profile: `?profile=` on `GET/POST /sessions`, `GET /sessions/search`,
  `GET/PATCH/DELETE /sessions/{id}` and `POST
  /sessions/{id}/attachments`, and `profile` in the `POST /runs` and `POST
  /sessions/{id}/regenerate` bodies. Unnamed, the profile the runtime
  serves, exactly as before. A conversation is found only under its own
  profile.
- **Resolution.** A run of the conversation is built from its profile
  through the run factory `lightagent chat` shares: persona, model, tool
  policy, durable memory (the profile's directory) and remote-credential
  grants (the profile's own). Nothing of the active profile is borrowed.
- **The active profile** changes only through `profile use` (CLI) or the
  desktop's profile switch (DESKTOP_ARCHITECTURE.md §7), never through a
  conversation's profile.
- **The desktop UI.** A Profile choice by the conversation list (shown
  when there is more than one profile) lists that profile's conversations
  and starts new ones as it. It is a per-viewer convenience kept in the
  page's storage, not a setting.

## Authorization: a profile id is a selector, never a credential

- **Local operator** (the desktop runtime's key, or an open loopback
  server): authorized as the operator first, by the host's key and the
  Host/Origin guard. It may then name any profile that exists in this
  home, as the CLI with access to the home may. A profile that does not
  exist is 404 and nothing is created; an invalid id is 400.
- **Multi-user mode** (identity on): a user's request that names a
  profile is authorized server-side (`authorize_profile`) against the
  user's grants, and a conversation is returned only if the user owns it;
  otherwise the answer is the same as for a conversation that does not
  exist. Holding, guessing or being sent a profile id or a conversation
  id grants nothing.

## Three identities, kept apart

| Identity | What it is | Local today | Never implies |
|---|---|---|---|
| Account | who acts (a person or a service) | `PrincipalId` in multi-user mode; the local operator otherwise | any profile |
| Agent profile | as which agent a run acts: persona, model, tools, memory, credential grants | `ProfileId` | an account, or ownership |
| Conversation | one transcript and its runs | `SessionId`, inside one profile's store; `owner` in multi-user mode | its profile's other conversations |

An account may be granted several profiles; a profile is used by an
account only through a grant; a conversation belongs to exactly one
profile and, in multi-user mode, one owner.

## The future cloud mapping (contract for the online stage)

Not implemented. The online stage must keep these rules:

1. **Keys.** A cloud conversation is addressed by (account, profile,
   conversation). Conversation ids are assigned by the server; a client
   never chooses an owner.
2. **Every request is authorized on the server**: authenticate the
   account; resolve its grants for the named profile; load the
   conversation only if its owner is that account and its profile is the
   named one; otherwise answer as for a missing conversation.
3. **Profiles map explicitly.** A local profile is linked to a cloud
   profile only by a user's explicit choice; ids are never matched by name
   alone, and an unlinked local profile is never uploaded.
4. **Synchronization** moves conversations of linked profiles only, with
   a revision per conversation (as `ConfigRevision` does for settings):
   a stale write is refused and offered for review, never merged
   silently.
5. **Credentials are never synchronized in plaintext.** Remote-credential
   grants stay per profile and per installation; the cloud stores at most
   that a grant exists. Provider keys stay with the installation.
6. **Local first.** Without an account, everything above works locally as
   it does today; signing out leaves local conversations local.

## Evidence (row 57)

- Backend: `crates/lightagent/tests/desktop_conversation_profile.rs`
  (persona and model of the bound profile; `chat --profile` sends the
  same; `chat --profile --session` and the desktop continue each other;
  the active profile unchanged; found only under its own profile; a
  missing or invalid profile finds and creates nothing).
- Host: `desktop/e2e/runs.py` `conversation_profile`.
- Browser: `e2e/conversation-profile.mjs`.
