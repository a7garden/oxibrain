# Oxi Foundation v1 — On-Disk Schema and Host Rules

> **Version:** v1 · **Date:** 2026-08-17
> **Status:** Frozen for v1. Any change to a JSON shape, an abstract requirement, or a
> rejection rule below requires a v2 contract and a new `schema_version` literal.
> **Companion:** `doc/adr/ADR-007-oxi-foundation-contract.md` (rationale),
> `doc/ARCHITECTURE.md` §15.7 and §19.3 (boundary), `doc/CONSUMPTION_CONTRACT.md` §1.1
> (additive client surface).
> **Cross-host fixture corpus:** `tests/fixtures/oxi-foundation/v1/` (oxicode mirrors this
> path; consumers may compare against either tree).

---

## 0. What this document is

The Oxi Foundation v1 contract names the on-disk artifacts every host parses, the
parsing and rejection rules every host must apply, and the precedence order every host
must use. It is **not** a runtime crate and **not** a daemon. Each host parses the
schema itself and is checked against the shared fixture corpus.

A host that implements v1 must:

1. Read **only** `~/.oxi/foundation/v1/`. The `$OXI_FOUNDATION_HOME` environment
   variable overrides the prefix for test and deployment use only (honoured by
   `foundation.rs` and `foundation_package.rs`); the default of `~/.oxi/foundation/v1/`
   is the production path. The daemon's listening socket lives at
   `~/.oxi/brain/oxibrain.sock`; `$OXIBRAIN_SOCKET` overrides that path the same way.
2. Reject the file on any `schema_version` other than `1` and on any structural
   violation below. The host must not silently fix or coerce.
3. Never read a secret from `profiles.json`. The secret lives in the OS Keychain, behind
   the host's `SecretResolver`.
4. Never write into `~/.oxi/brain/`. The daemon is the sole writer there.

A host that does not implement v1 still works: the local-GGUF default
(`oxibrain-llm-local`) needs no Foundation input, and `oxibrain-client` continues to
serve the existing JSON-RPC surface unchanged.

---

## 1. Directory layout

```
~/.oxi/
├── foundation/v1/                 # this contract — non-secret
│   ├── profiles.json              # provider profiles (Keychain locator only)
│   └── packages.lock              # resolved Foundation packages
└── brain/                         # oxibrain store — daemon is the sole writer
    └── oxibrain.sock              # default listening socket
    # (also: $OXIBRAIN_SOCKET override; serve --daemon binds the default when --socket
    #  is absent)
```

The Foundation directory is world-readable. The Keychain is not.

`$OXI_FOUNDATION_HOME` overrides the foundation root (test/deployment only); both
`foundation.rs` and `foundation_package.rs` honour it. `$OXIBRAIN_SOCKET` overrides
the daemon's listening socket path. Production callers leave both unset.

---

## 2. `profiles.json` — provider profiles

A profile names a provider, a model, the roles the profile is permitted to satisfy, and
a `{service, account}` Keychain locator for the secret. Profiles are non-secret by
construction; any field that could carry a secret is a parse-level rejection.

### 2.1 Shape

```json
{
  "schema_version": 1,
  "profiles": [
    {
      "id": "<redacted>",
      "provider": "<redacted>",
      "model": "<redacted>",
      "roles": ["coding.primary", "assistant.general"],
      "credential": {
        "service": "<redacted>",
        "account": "<redacted>"
      }
    }
  ]
}
```

### 2.2 Fields

| Field | Type | Required | Notes |
|---|---|---|---|
| `schema_version` | integer | yes | Must equal `1`. Any other value is rejected. |
| `profiles` | array | yes | Empty array is valid; it means "no Foundation-provided profiles". |
| `id` | string | yes | Unique within the file. Duplicates are rejected. |
| `provider` | string | yes | Provider kind identifier. Resolved by the host's adapter catalogue. |
| `model` | string | yes | Model identifier as the provider knows it. |
| `roles` | array of string | yes | Roles this profile is permitted to satisfy. |
| `credential.service` | string | yes | OS-Keychain service name. |
| `credential.account` | string | yes | OS-Keychain account name. |

### 2.3 Allowed role values

Exactly these four:

- `memory.extract`
- `memory.consolidate`
- `coding.primary`
- `assistant.general`

Any other string in `roles` is rejected. The set is intentionally small and is the
contract between callers (which ask for a role) and profiles (which declare which roles
they may satisfy). A profile that lists a role the host does not know is rejected at
parse time so that typos do not silently disable a profile.

### 2.4 Credential locator abstraction

The `credential` field is **only** a Keychain locator. The shape is fixed at exactly
`{"service": "<string>", "account": "<string>"}`. The host's `SecretResolver` resolves
that pair to a secret via the OS Keychain (macOS Keychain, Linux `libsecret`, Windows
Credential Manager, or the test deterministic store). Hosts must never read the secret
from `profiles.json`, never inline a secret into a profile, and never log the resolved
secret. The locator is *safe to share*; the secret is not.

### 2.5 Invalid-profile rejection

A profile is rejected at parse time (before any adapter is constructed) when:

- `schema_version` is not `1`.
- The JSON does not parse, or any required field above is missing.
- The same `id` appears twice in `profiles`.
- `roles` is empty, contains an unknown role, or contains a duplicate.
- `provider` or `model` is the empty string.
- `credential.service` or `credential.account` is the empty string.
- The profile carries any field whose name suggests a secret: `api_key`, `apikey`,
  `api-token`, `bearer`, `access_token`, `refresh_token`, `secret`, `password`,
  `private_key`. (The exact list is part of the contract and is fixture-tested.)

A rejected file means the host uses its fallback path (existing
`ANTHROPIC_*`/`OPENAI_*` environment variables, then local model). It does **not**
silently send extraction to a different remote provider.

### 2.6 Example — rejected profile (carries a secret-shaped field)

```json
{
  "schema_version": 1,
  "profiles": [
    {
      "id": "<redacted>",
      "provider": "<redacted>",
      "model": "<redacted>",
      "roles": ["coding.primary"],
      "credential": { "service": "<redacted>", "account": "<redacted>" },
      "api_key": "<redacted>"
    }
  ]
}
```

The `api_key` field makes the entire file invalid. The host falls through to its
fallback path and reports why.

### 2.7 Example — locator only (the canonical shape)

```json
{
  "schema_version": 1,
  "profiles": [
    {
      "id": "work-summariser",
      "provider": "<redacted>",
      "model": "<redacted>",
      "roles": ["memory.consolidate", "assistant.general"],
      "credential": { "service": "<redacted>", "account": "<redacted>" }
    }
  ]
}
```

All sensitive identifiers (provider, model, service, account) are redacted. The
structure is what matters.

---

## 3. `packages.lock` — resolved Foundation packages

A `packages.lock` records which immutable Foundation packages a host has resolved, with
their `name`, `version`, content `digest`, `source`, `trust`, the `targets` they apply
to, and the abstract `requirements` they declare. Hosts do not execute arbitrary code
from a Foundation package; they verify the digest, map `requirements` to host resources,
and apply their own scope/approval/audit policy.

### 3.1 Shape

```json
{
  "schema_version": 1,
  "packages": [
    {
      "name": "@oxi/code-review",
      "version": "1.4.0",
      "digest": "sha256-<64 lowercase hex>",
      "source": "foundation",
      "trust": "verified",
      "targets": ["oxicode"],
      "requirements": ["workspace.read", "workspace.patch", "brain.query"]
    }
  ]
}
```

### 3.2 Fields

| Field | Type | Required | Notes |
|---|---|---|---|
| `schema_version` | integer | yes | Must equal `1`. |
| `packages` | array | yes | Empty array is valid; it means "no Foundation packages". |
| `name` | string | yes | Package name. The namespace prefix `@oxi/` is conventional, not required. |
| `version` | string | yes | SemVer string. |
| `digest` | string | yes | `sha256-` followed by 64 lowercase hex characters. |
| `source` | string | yes | Origin identifier (e.g. `foundation`, a registry URL). |
| `trust` | string | yes | One of `verified`, `pinned`, `untrusted`. See §3.4. |
| `targets` | array of string | optional | Hosts the package applies to. Absent = all. |
| `requirements` | array of string | yes | Abstract capabilities the package needs. See §3.3. |

### 3.3 Abstract requirements

A package declares only abstract capabilities. The full set of legal values is:

- `workspace.read`
- `workspace.patch`
- `shell.execute`
- `browser.navigate`
- `brain.query`
- `schedule.manage`

Any other string is **not** a parse-level rejection: the reader preserves
unknown requirement strings verbatim (§3.7) so a package's declaration can
never be hidden by a parse. The host's policy decides what to do with a
string outside the closed set — reject the package, escalate to approval,
or map to a no-op. The contract does not pick.

Abstract requirements are mapped to host resources by the host's own
policy; the contract does not grant capabilities. A package that lists
`shell.execute` does **not** receive shell execution; the host maps that
to its own approval flow and may deny it. Workspace and project overlays
remain host-local and higher precedence than the shared immutable registry.

### 3.4 Digest and trust rules

- `digest` must be exactly `sha256-` followed by 64 lowercase hex characters. Any other
  form is rejected.
- The host verifies the digest against the package's source before loading it. A
  mismatch is a hard error.
- `trust` is one of:
  - `verified` — the host has confirmed the digest against a published signature.
  - `pinned` — the host has pinned the digest from a trusted channel but has not
    re-verified it on this run; treated as `verified` for read-only flows and as
    `untrusted` for write-bearing flows.
  - `untrusted` — the host has not verified the digest; the package is loaded only
    inside the host's sandbox, with `requirements` mapped to a no-op or a read-only
    resource.
- A package cannot grant capabilities, bypass a host approval flow, or cause every
  package to be injected into every prompt. These are contract-level prohibitions.

### 3.5 Example — rejected lock (bad digest prefix)

```json
{
  "schema_version": 1,
  "packages": [
    {
      "name": "@oxi/code-review",
      "version": "1.4.0",
      "digest": "sha1-<not-a-valid-sha256>",
      "source": "foundation",
      "trust": "verified",
      "targets": ["oxicode"],
      "requirements": ["workspace.read"]
    }
  ]
}
```

The `digest` field's prefix is not `sha256-`. The file is rejected at parse time.

### 3.6 Example — abstract requirements, no grants

```json
{
  "schema_version": 1,
  "packages": [
    {
      "name": "<redacted>",
      "version": "1.0.0",
      "digest": "sha256-<64 lowercase hex>",
      "source": "<redacted>",
      "trust": "verified",
      "targets": ["<redacted>"],
      "requirements": ["brain.query"]
    }
  ]
}
```

The structure is what matters. Note `brain.query` is an **abstract** requirement: it
says "I need to ask the brain", not "I have permission to ask the brain". The host's
own policy decides whether that maps to a real `BrainClient` call.

### 3.7 Typed reader and unknown-requirement preservation

`oxibrain-client` ships a typed reader at `crates/oxibrain-client/src/foundation_package.rs`
(`pub mod foundation_package`, re-exported as `oxibrain_client::foundation_package`).
The reader exposes two document kinds and one cross-check:

1. **Lockfile reader** — `parse_packages_lock` / `load_packages_lock`
   consume `packages.lock` and enforce the rejection rules in §3.2 / §3.4:

   - `schema_version == 1` is enforced at the top level, *before* any
     package body is parsed. Any other value rejects the whole file
     with a typed error.
   - `digest` is exactly `sha256-` + 64 lowercase hex characters.
     Uppercase hex, wrong prefix, or wrong length is rejected.
   - `trust` is one of `verified`, `pinned`, `untrusted`.
   - `targets` (`None` means "all targets") and `requirements` are
     parsed; `AbstractRequirement::is_known()` lets the host's policy
     code reject, ignore, or escalate.

2. **Manifest reader** — `parse_package_manifest` / `load_package_manifest`
   consume a single package's `manifest.json`. The manifest shape is
   `name`, `version`, `digest`, optional `targets`, optional `persona`
   (name + description), optional `payloads` (each `{"kind":"inline",
   "value":...}` or `{"kind":"path","value":...}`), and `requires`.
   The same digest-format validation applies. `requires` is the
   manifest spelling of what the lockfile calls `requirements`; both
   spellings appear in the contract because the manifest is the source
   of truth and the lockfile is the resolved view.

3. **Cross-check** — `PackageManifest::matches_lock_entry(&entry)`
   confirms the manifest and lock entry describe the same package.
   Per §3.4, a manifest digest that disagrees with the lockfile's
   digest is a **hard error**: the host must refuse to load the
   package before executing anything from it.

If a package lists a requirement string outside the closed set in §3.3, the
reader **preserves** it as `AbstractRequirement::Unknown(s)` rather than silently
dropping the package or the requirement. Hosts may choose to reject the
package, surface the requirement for human approval, or map it to a no-op;
the contract does not pick. The point is that a parse never hides what a
package declared.

`brain.query` is the only requirement that maps to the brain. It maps **only**
to existing scoped Brain *read* operations. `brain.ingest`, `brain.declare`,
`brain.retract`, and `brain.redact` are not package-grantable at all; a
package that lists them is rejected by the host's policy as a privilege
escalation attempt. The reader carries the string verbatim so a host's
policy can detect and reject it.
### 3.8 Helper invariants

Every helper in `foundation_package` is pure and read-only:

- `select_package_for_target(&lock, target)` returns the first
  matching lock entry (or `None`). It does not write to
  `packages.lock`.
- `load_packages_lock(home)` reads `<home>/packages.lock` only.
- `load_package_manifest(home, name)` reads
  `<home>/manifests/<name>.json` only.
- `parse_package_manifest(&manifest).matches_lock_entry(&entry)`
  compares two already-parsed values in memory; it does not touch
  disk.

None of these helpers:

- opens the brain socket;
- consults, broadens, or bypasses the daemon's `Scope`;
- mutates anything on disk in the Foundation home or elsewhere.



A Read-only scope on the host's `BrainClient` cannot be widened through
any helper in `foundation_package`. The reader has no bridge to a
scope in the first place — authority over scope stays with the daemon
and the host's token resolver.

## 4. Role-binding rules

A profile is selected by **role**, not by provider. The host asks "what role do I need
to satisfy?" and the host's resolver returns a profile whose `roles` contains that
value, subject to the precedence in §6. A host that needs multiple roles may select
multiple profiles.

The legal role set is the four values in §2.3. A profile that lists a role the host
does not know is rejected (§2.5).

The role-binding rule means a caller never names a provider or a profile ID directly:
it names a role, and the contract decides which profile (if any) is bound to that
role. That is what keeps "memory extraction goes to a small model; user-facing chat
goes to a frontier model" expressible as data rather than code.

---

## 5. Keychain-locator abstraction

The host's `SecretResolver` is a small trait at the facade/CLI boundary. It has two
implementations:

- **Production** — resolves `{service, account}` against the platform OS Keychain
  (macOS Keychain, Linux `libsecret`, Windows Credential Manager).
- **Test / deterministic** — returns secrets from a fixed map, used by the fixture
  corpus and the host's own tests.

The trait's only contract is: *given a validated locator, return a secret, or return a
typed error explaining the failure.* The trait never returns a serialized credential,
never writes to disk, and never logs the secret. `oxibrain-core`, `oxibrain-store`, and
`oxibrain-index` never see a `SecretResolver` — they receive an `LlmPort` (or
`EmbeddingPort`) that is already wired.

A missing or unavailable Keychain secret is reported to the caller with a typed error.
The host then either:

- retries with a different profile that can satisfy the role, if policy permits;
- falls through to the next precedence level (see §6); or
- surfaces the failure to the user.

It does **not** silently send extraction to a different remote provider.

---

## 6. Precedence — how a request becomes a profile

The host resolves a role in this order:

1. **Explicit override.** The host's CLI flag, configuration, or environment variable
   names a specific profile ID. Validated against the loaded profiles. **An explicit
   override that names an unknown profile ID, provider, or model is a hard error — not
   a fall-through.** Step 1 is the strict path; only steps 2–4 fall through on
   absence.
2. **Foundation profile for the role.** A profile whose `roles` contains the requested
   role, selected by host-defined priority (currently: first match in `profiles[]`).
   The Keychain secret must be resolvable for the profile to be selected.
3. **Compatibility environment variables.** Existing `ANTHROPIC_*` / `OPENAI_*` /
   `OLLAMA_*` variables, unchanged from before the Foundation contract.
4. **Local model.** The default `oxibrain-llm-local` GGUF adapter.

A failure at any level falls through to the next, except where host policy says
otherwise (a profile marked `verified` may, by host policy, refuse to fall through to a
remote provider at level 3). The exception for step 1 is the explicit-override
strictness above; the fall-through rule applies to steps 2–4 only.

---

## 7. Host-capability rule

A Foundation package declares `requirements`. The host maps each requirement to a
**host capability** through its own policy:

| Abstract requirement | Typical host capability |
|---|---|
| `workspace.read` | read-only access to a host-defined workspace root |
| `workspace.patch` | apply patches the host has approved through its own approval flow |
| `shell.execute` | a sandboxed shell, only after host policy has approved it |
| `browser.navigate` | a host-mediated browser tab, not raw network |
| `brain.query` | a `BrainClient` connection to the local daemon (default socket) |
| `schedule.manage` | the host's own scheduler, never a Foundation-side one |

The mapping is **the host's call**, not the Foundation contract's. Two hosts may map
the same requirement differently. The contract only names the abstract requirement;
authority over granting it stays with the host. This is the rule that keeps "every
package can execute shell" from being a contract-level possibility.

---

## 8. Discovery and capability handshake

This is the additive `oxibrain-client` surface described in `doc/CONSUMPTION_CONTRACT.md`
§1.1. Restated briefly:

- The daemon's listening socket is `~/.oxi/brain/oxibrain.sock` by default, or the path
  in `$OXIBRAIN_SOCKET`. `serve --daemon` binds the default when `--socket` is absent.
- The client opens a JSON-RPC connection and, before any payload, exchanges
  `ClientHello` (carrying `client_version`, `protocol_version`, `supported_features`)
  with `ServerInfo` (carrying `server_version`, `schema_version`,
  `supported_features`, `requires_client_features`).
- Discovery and negotiation ride this handshake. The MCP tool surface stays at
  fifteen; capability negotiation is not a sixteenth tool.
- Auth-first-message is preserved: the token (or the anonymous-on-Unix-socket flag)
  comes after `ServerInfo` and before any payload. The `Scope`/`Capability` model in
  `ARCHITECTURE.md` §15.1–§15.2 is unchanged. Discovery metadata never replaces a
  token and never broadens scope.

---

## 9. Cross-host fixture corpus

Path: `tests/fixtures/oxi-foundation/v1/`. The corpus is **byte-identical** across
hosts (oxicode and oxibrain check it into the same path with the same contents); if
a host can parse a v1 fixture that another host rejects — or vice versa — the
contract has drifted and must be reported, never silently edited.

Layout (the canonical ten files):

```
tests/fixtures/oxi-foundation/v1/
├── foundation.json                       # host-compatibility declaration (this contract version)
├── profiles/
│   ├── valid_personal_coding.json        # parse: accept  (one profile, two roles)
│   ├── unknown_schema.json               # parse: reject  (schema_version != 1; §2.5)
│   ├── duplicate_profile_id.json         # parse: reject  (id appears twice; §2.5)
│   ├── malformed_credential_locator.json # parse: reject  (empty credential fields; §2.5)
│   └── role_ambiguous.json               # parse: accept  (two profiles share `coding.primary`;
│                                         #               resolution chooses first; §6)
└── packages/
    ├── valid_lock.json                   # parse: accept  (closed-set requirements, sha256 lowercase)
    ├── bad_digest.json                   # parse: accept  (well-formed lock; digest mismatch with
                                          #               the published source is install-time,
                                          #               not a parse-time rejection; §3.4)
    ├── missing_target.json               # parse: accept  (`targets: ["oxibrain"]` makes it
                                          #               invisible to oxicode / oxios callers;
                                          #               host policy decides; §3.4)
    └── denied_requirement.json           # parse: accept  (`requirements: ["kernel.modify"]` is
                                          #               preserved as `Unknown`; §3.3, §3.7)
```

The expected outcome column is **part of the contract** and is exercised by
every host's parser tests. Hosts may add additional local-only fixtures, but the
canonical ten above must parse with the stated outcome on every host. A parser
that disagrees with the table above is a contract-drift bug — file a report and
do not edit the fixture.

### 9.1 Capabilities rejection is resolution-time, not parse-time

A Foundation profile declares its remote model's capabilities (e.g. `tool_call`,
`json_schema`) under `declared_capabilities`. The parser always accepts the
profile — declared capabilities are just data. The
`FoundationError::CapabilityUnsatisfied` rejection fires only at role+mechanism
resolution time (e.g. `ResolvedProfiles::pick_for_role(role, mechanism)` in the
oxibrain-cli crate, the equivalent in oxicode / oxios): the strict parser does
not know the caller's role, and a host with no role/mechanism resolver could
not share a parse-time outcome for it.

This is why the canonical ten fixtures deliberately carry **no** "unsupported
model capability" entry: the contract has one rejection rule that no shared
fixture can capture. The rule is covered by the local unit test
`pick_for_role_rejects_when_capabilities_unsatisfy` in
`oxibrain-cli/src/cmd/foundation.rs` and the equivalent tests in oxicode /
oxios; any change to that rule is a schema change that requires a v2 contract.

---

## 10. Stability

The shapes, role values, abstract requirements, locator shape, rejection rules, and
precedence order above are **frozen** for v1. A change to any of them requires a
v2 contract and a `schema_version` literal of `2` in the relevant file. The fixture
corpus is the test: if a host can parse a v1 fixture that another host rejects (or
vice versa), the contract has drifted.
