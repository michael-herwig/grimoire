# Plan — `grim login` / `grim logout`

## Status

- **Plan:** plan_login_logout
- **Active phase:** 3 — Review-Fix Loop (converged, round 1)
- **Step:** awaiting /finalize (ready for human review; not committed)
- **Last update:** 2026-06-01 (review-fix applied; full `task verify` green)

### Review-fix loop (round 1)

Adversarial review (5 dimensions, per-finding verify) → 15 confirmed
(3 block / 9 warn / 3 suggest). Fixed all actionable:

- **block** helper stdout/stderr log leak (CWE-532) → `map_helper_err`
  redacts `HelperFailure` into a payload-free `AuthError::HelperFailed`.
- **block** blocking TTY/stdin reads on the async worker → `input_task`
  (`spawn_blocking`).
- **block** panic in `spawn_blocking` masked as `StoreIo` → `resume_unwind`.
- **warn** logout TOCTOU (`path.exists()` before lock) → lock-free read;
  lock taken only for the plaintext write.
- **warn** `classify_auth` helper-variant coverage → explicit arms.
- **warn** missing classify tests → added (auth + command paths).
- **warn** stdin read error → classified `StoreIo` (IoError 74, not 1).
- **warn** dead `auth.rs` re-exports → removed.
- **warn** inaccurate `dup_credential` doc → corrected.
- **warn** transient cleartext on heap → `Zeroizing` (stdin buf + `user:pass`).
- **suggest** flock held across the 30s helper subprocess → helper path is
  now lock-free (only the plaintext `auths` write takes the lock).

Deferred (out of this diff's scope): double error log in `main.rs`
(pre-existing); `print_json` DRY across 10+ pre-existing report types.

## Goal

Add `grim login` / `grim logout` so credentials can be **written** to the
docker-compatible credential store. Grimoire already *reads* credentials
(`oci::access::registry_client::auth_for` via `docker_credential`); only the
write/erase half was missing.

Adheres to Grimoire's simpler style — does **not** copy OCX's architecture
(no lib/CLI workspace split, no `RegistryPing` verify seam in v1, no
sticky-detect of the native helper). Reuses OCX's **patched external
repository** (the `docker_credential` fork) and its login/logout shape.

## Decisions (confirmed with maintainer)

| Decision | Choice | Rationale |
|---|---|---|
| Fork vendoring | Git submodule `external/docker_credential` (ocx-sh fork) + `[patch.crates-io]`, pinned at `8e89cd0` (`feat/store-erase-list`) | Single source of truth; matches OCX. Only grimoire depends on `docker_credential` (oci-client 0.16 does not) → patch is self-contained |
| Password input | `--password-stdin` **and** a hidden TTY prompt (`rpassword`) | Ergonomic like `docker login`, still no argv-visible secret |
| Secret type | `secrecy::SecretString` | Redacts from `Debug`, zeroizes on drop |
| Credential verify | None in v1 (store optimistically) | Matches `docker login` w/ helper; KISS. Function shape leaves room for a future `--verify` |

## What landed

- **Submodule** `external/docker_credential` + `[patch.crates-io]`; deps
  `secrecy`, `rpassword`, `base64`.
- **`src/auth/`** subsystem (named module files, no `mod.rs`):
  `registry_url` (canonicalize), `credential` (secrecy), `auth_error`
  (thiserror, `#[non_exhaustive]`), `store` (`CredentialStore` trait +
  `DockerCredentialStore` over `~/.docker/config.json`, reuses
  `ConfigFileLock` + `atomic_write`, enforces `0600`, drops OCX
  sticky-detect), `prompt` (TTY username + hidden password), `login`
  (login/logout ops).
- **Commands** `src/command/login.rs`, `logout.rs`; report
  `src/api/login_report.rs` (single-table plain, single-object JSON).
- **Wiring**: `Command::{Login,Logout}` in `main.rs`/`app.rs`;
  `Error::Auth(#[from] AuthError)` + `classify_auth` in `error.rs`;
  `CommandError::{NoLoginRegistry, LoginInput}`; shared
  `command::resolve_login_registry` / `login_usage`.
- **Read/write key parity**: `registry_client::auth_for` now canonicalizes
  its lookup key (fixes the `docker.io` alias round-trip); fork's new
  `NotFound` helper-miss variant folded into the anonymous group.
- **Docs**: `DOCKER_CONFIG` in CLAUDE.md env table; login/logout in the
  command catalog.

## Exit-code contract

| Situation | Code |
|---|---|
| Success | 0 |
| Missing/empty credential input (non-interactive) | 64 usage |
| Malformed on-disk config | 65 data |
| Store I/O failure | 74 io (77 if `EPERM`) |
| No helper + no `--allow-insecure-store`; no config location; no registry | 78 config |
| Helper auth failure / login rejected | 80 auth |

## Verification

- `cargo test`: 455 unit tests pass (incl. new auth/store/command tests).
- `test/tests/test_login.py`: 12 acceptance tests pass (plaintext round-trip
  + `0600`, refusal w/o opt-in, usage errors, JSON, logout no-op, mock
  native-helper store/erase).
- Full `task verify` green (fmt, clippy, license, build, 455 unit + 78
  acceptance, shell, claude config tests, link check).

## Out of scope (v1)

Credential pre-verify (`--verify`/Ping), native-helper auto-detect, OAuth /
browser flows, `--insecure` HTTP toggle for the (absent) verify path.
