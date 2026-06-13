# Research: install-state portability

Supports `adr_install_state_portability.md` (relocate + anchor-relativize
install state for shared `GRIM_HOME` / devcontainers). Three axes:
A) anchored paths & traversal safety, B) lock/state split & devcontainer
portability, C) schema migration & versioning.

## Axis A: anchored paths & traversal safety

**Date:** 2026-06-13
**Context:** Grimoire stores install records as `(PathAnchor, relative: String)` pairs
(forward-slash UTF-8) per ADR `adr_install_state_portability.md`. At resolve
time the stored relative is re-joined under a trusted anchor root that may
differ between host and devcontainer. The stored relative is **untrusted input**:
a corrupt or adversarially crafted state file could contain `../` sequences.

### Approach comparison

| Approach | Works w/o path existing | Symlink-safe | Windows-safe | Verdict |
|---|---|---|---|---|
| `Component` filter (reject non-`Normal`) | Yes | Partial (lexical only) | Yes (also filter `Prefix`) | **Use as Layer 1** |
| `canonicalize` + `starts_with` | No (errors if absent) | Yes (resolves symlinks) | With `dunce` only | **Use as Layer 2** |
| `cap-std::Dir` | Yes | Yes (kernel-enforced) | Yes | Overkill for this use case |
| `relative-path` `to_path` / `to_logical_path` | Yes | No | Yes | Storage type only; no security |
| `dunce::canonicalize` alone | No | No (no containment check) | Yes | Normalization helper, not guard |

### Pitfalls

- **`canonicalize` requires path exists.** Guard with `candidate.exists()` before
  calling; validate-only Layer 1 covers non-existent paths.
- **`starts_with` must be Path-level, not string-level.** `Path::starts_with` is
  component-granular (correct). String `str::starts_with` is a byte prefix (wrong:
  `/foo/bar-extra` matches `/foo/bar`).
- **Windows UNC mismatch.** `std::fs::canonicalize` emits `\\?\C:\…`; comparing
  with a plain `C:\…` anchor produces false negatives. Use `dunce::canonicalize`
  on both sides (no-op on Unix).
- **`CurDir` (`.`) is harmless** but `ParentDir` (`..`) and `RootDir` (`/`) must
  be rejected. Windows `Prefix` (`C:`) must also be rejected.
- **TOCTOU window.** Between Layer 1 check and filesystem op, a symlink could be
  replaced. Layer 2 canonicalize-check shrinks the window but cannot eliminate it
  without a kernel-level capability (`cap-std::Dir`). Acceptable for local
  install-state (not a multi-tenant sandbox).
- **CVE-2024-51756** (cap-primitives < 3.4.1): even the heavy `cap-std` solution
  had a bypass via superscript-digit device filenames. Fixed in 3.4.1.

### Separator normalization

Store as forward-slash UTF-8 `String` (what `relative-path::RelativePathBuf`
serializes). At resolve time, `Path::new(stored_rel)` accepts `/` on all
platforms. Component iteration normalizes separators automatically.

### Canonicalize timing

- **At store time:** validate relative is `Normal`-only; record clean `String`.
- **At resolve time:** `canonicalize` + `starts_with` only when path exists.
- Do NOT canonicalize the stored relative itself — that would require the path
  to exist at record time and would bind it to the current host's symlink
  topology.

### Recommended `resolve` implementation

```rust
use std::path::{Component, Path, PathBuf};

pub fn resolve(anchor: &Path, stored_rel: &str) -> Result<PathBuf, ResolveError> {
    // Layer 1: lexical guard (works for absent paths, no TOCTOU).
    let rel = Path::new(stored_rel);
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            other => return Err(ResolveError::TraversalAttempt(
                format!("{other:?} in {stored_rel:?}")
            )),
        }
    }
    let candidate = anchor.join(rel);

    // Layer 2: symlink-escape guard (only when path exists).
    if candidate.exists() {
        // dunce avoids \\?\ UNC on Windows; no-op on Unix.
        let ca = dunce::canonicalize(anchor).map_err(ResolveError::Io)?;
        let cc = dunce::canonicalize(&candidate).map_err(ResolveError::Io)?;
        if !cc.starts_with(&ca) {
            return Err(ResolveError::EscapedAnchor { anchor: anchor.to_owned(), resolved: cc });
        }
    }
    Ok(candidate)
}
```

**Deps:** `dunce = "1"` (no other crates needed). Optionally swap `String` field
in `AnchoredPath` for `relative_path::RelativePathBuf` (serde feature) for
type-level documentation of the forward-slash invariant.

### Key takeaways

1. Two-layer pattern (lexical Component filter + canonicalize/starts_with) is the
   canonical Rust idiom. No crate beyond `dunce` required.
2. `dunce` is load-bearing on Windows — skipping it silently passes escaped paths.
3. `relative-path::RelativePathBuf` is the right storage type (self-documents the
   forward-slash invariant); bare `String` works too.
4. `cap-std` is overkill (sandbox-grade); proportionate only for untrusted multi-tenant.
5. Canonicalize at resolve time only, never store time.
6. `Path::starts_with` (component-granular), never `str::starts_with`.

### Sources

- [std::path::Component](https://doc.rust-lang.org/std/path/enum.Component.html)
- [relative-path docs](https://docs.rs/relative-path/latest/relative_path/index.html)
- [dunce docs](https://docs.rs/dunce/latest/dunce/) — Windows UNC fix
- [safe-path::scoped_join](https://docs.rs/safe-path/latest/safe_path/fn.scoped_join.html)
- [CVE-2024-51756 cap-primitives](https://security.snyk.io/vuln/SNYK-RUST-CAPPRIMITIVES-8344251)
- [rust-lang/rust #42869](https://github.com/rust-lang/rust/issues/42869) — UNC motivation for dunce
- [soft-canonicalize](https://docs.rs/soft-canonicalize/latest/soft_canonicalize/) — canonicalize for non-existent paths

## Axis B: lock/state split & devcontainer portability

### 1. Industry Precedent — Resolution vs. Machine-Local Install Facts

Every mature package manager draws the same two-file boundary. The committed file records **what** (resolution intent, digests, version pins). The machine-local layer records **where** and **whether** — materialized artifacts and filesystem facts that differ per machine.

| Tool | Committed (resolution) | Machine-local (install facts) | Stores absolute paths? |
|------|------------------------|-------------------------------|------------------------|
| **Cargo** | `Cargo.lock` — pinned dependency graph, no paths | `target/.fingerprint/` — dep-info `.d` files, incremental state | Yes, `.d` dep-info files embed absolute host paths; `build.dep-info-basedir` can relativize them. `LocalFingerprint` hashes avoid absolutes. Missing `target/` = full rebuild. |
| **npm** | `package-lock.json` — exact version + integrity tree | `node_modules/.package-lock.json` — hidden lockfile v3, install-time optimization cache | Top-level lock stores no filesystem paths. Hidden lock is an internal perf artifact; invalidated if tree modified by other tools. Missing = `npm install` reads top-level lock and re-materialize. |
| **pnpm** | `pnpm-lock.yaml` — resolved graph, relative dep keys for default-registry packages | `node_modules/.modules.yaml` — store path, hoisting layout | Registry deps use relative keys. **Local-path deps use absolute paths** — tracked as a bug in pnpm/pnpm#8723 and #9794. The content store (`~/.pnpm-store`) holds hardlinked blobs at absolute paths, never committed. |
| **Poetry** | `poetry.lock` — pinned packages, hashes, markers | `.venv/` — machine-local interpreter tree | Lock stores no filesystem paths for registry deps. Path dependencies with extras accidentally serialize absolute paths in some versions (poetry#9128 — open bug). `.venv` is gitignored and rebuilt from the lock. |

**Key takeaway across all four:** The committed lock is machine-independent by design — no absolute host paths, no install-location facts. The machine-local layer is either gitignored entirely (`.venv`, `node_modules`, `target/`) or a cache dir outside the repo (`~/.pnpm-store`, `~/.cargo/registry`). None store "which users have this installed and where exactly" in the committed lock — that is always machine-local. The ADR's Option 1 mirrors this universal split.

### 2. Devcontainer Conventions — What Travels, What Stays Local

**Bind mount semantics.** VS Code devcontainers mount the git-repo root into the container as a bind mount, typically at `/workspace`. A Docker bind mount exposes the **raw host filesystem directory** — it is NOT filtered by `.gitignore` or `.dockerignore` (gitignore is a git concept; bind mounts are kernel-level). Therefore **gitignored files present on disk at mount time ride into the container**. A host `.grimoire/state.json` is visible at `/workspace/.grimoire/state.json` with no extra config.

**What the ecosystem isolates with named volumes:** `node_modules`, `.venv`, `.terraform` — large, write-heavy, machine-specific, cheaply rebuildable from the committed lock; mounted over by a named volume for I/O speed. grim's `.grimoire/state.json` is small, not write-heavy, and NOT cheaply rebuildable (the state IS the record). Profile matches **bind-mount travel**, not named-volume isolation. The ADR's "travels via the /workspace bind mount" claim is correct.

### 3. Risk Analysis — Absent, Stale, or Read-Only State

- **Fresh clone / CI (absent):** normal case. `grim install` detects no state and creates it (does real work: fetch/render/write), exactly like `cargo build`/`npm install` from a lock. CI must run `grim install` explicitly — correct contract.
- **Stale (artifacts removed/modified):** the `content_hash` per output guards it — `grim status` re-hashes at `resolve(anchor, relative)`; absent ⇒ `missing`, edited ⇒ `modified`. No silent corruption.
- **Read-only `/workspace` mount:** `grim install` cannot write `.grimoire/state.json` — real CI failure mode. Mitigations: mount only `grimoire.toml`/`grimoire.lock` read-only, or add a `GRIM_STATE_DIR` override to a writable path. **Not covered by ADR — flag as follow-up.**
- **Two containers sharing one bind-mounted workspace:** concurrent `grim install` races on `state.json` — needs the advisory write lock the ADR already mandates.

### Opinionated takeaways for grim

- **Confirm `.grimoire/state.json` in workspace** — matches every precedent (machine-local facts, co-located, in-tree+gitignored, delivered to devcontainers free via bind mount).
- **Confirm gitignored** — committing causes VCS churn across teammates with different clients (same reason Cargo never commits `target/`). The gitignore entry is load-bearing.
- **Rebuild-on-missing is the recovery story** — `grim install` on missing state behaves as first install.
- **Never put absolute paths in the lock** — pnpm/Poetry both have open bugs leaking absolutes; the `grimoire.lock` must hold only registry digests. `AnchoredPath` in state is the correct guard.
- **Flag read-only workspace** — consider `GRIM_STATE_DIR` override (small addition, removes the only plausible Option-1 failure mode).
- **Gotcha — named-volume shadowing:** if a consumer mounts a named volume at `/workspace/.grimoire`, the bind-mounted `state.json` becomes invisible (volume wins) — silently breaks the "travels via bind mount" invariant. Document: do not place a named volume at this path.

### Sources

- [Cargo Build Cache](https://doc.rust-lang.org/cargo/reference/build-cache.html); [cargo fingerprint](https://doc.rust-lang.org/nightly/nightly-rustc/cargo/core/compiler/fingerprint/index.html)
- [package-lock.json](https://docs.npmjs.com/cli/v11/configuring-npm/package-lock-json/)
- [pnpm#8723](https://github.com/pnpm/pnpm/issues/8723), [pnpm#9794](https://github.com/pnpm/pnpm/issues/9794), [poetry#9128](https://github.com/python-poetry/poetry/issues/9128)
- [Docker bind mounts](https://docs.docker.com/engine/storage/bind-mounts/); [VS Code default source mount](https://code.visualstudio.com/remote/advancedcontainers/change-default-source-mount); [VS Code perf / named volumes](https://code.visualstudio.com/remote/advancedcontainers/improve-performance); [devcontainers/spec #104](https://github.com/devcontainers/spec/discussions/104)

## Axis C: schema migration & versioning

### 1. Versioned-envelope patterns in serde

Grimoire already uses the right primitive: `serde_repr` on a `#[repr(u8)]` closed enum auto-rejects unknown discriminants with zero hand-rolled validation. Current `InstallStateVersion::V1 = 1` mirrors Cargo's `ResolveVersion`.

| Pattern | Mechanism | Verdict |
|---|---|---|
| `serde_repr` bump (V1=1, V2=2) | envelope `version` drives a `match`; each arm deserializes the right wire struct | **Recommended** — zero new deps, matches existing convention |
| `#[serde(tag = "version")]` internally tagged | JSON field selects variant | collides with `deny_unknown_fields` (serde#2666); needs struct-variants |
| `#[serde(untagged)]` try-new-then-old | buffers whole payload per attempt; first success wins | **Avoid** — re-buffering (serde#2101) + error becomes "data did not match any variant", structural mismatch silently discarded |

**Recommended V2 envelope:** two wire structs (`InstallStateFileV1` old shape / `InstallStateFileV2` new shape) + a minimal `VersionProbe { version }` peeked first; `match probe.version` dispatches. Two deserializations of a few-KB file is negligible; no untagged buffering, full error fidelity.

### 2. Lazy convert-on-load vs explicit migration — the drift-baseline problem

**Critical risk:** a naive lazy rebuild re-hashes whatever bytes are on disk and records that as the new `content_hash`. If the user edited the artifact, their edited hash becomes the new baseline — the drift guard silently resets to "clean", and a later `grim update` overwrites the customization with no warning. Structurally identical to the "forward-only migration with silent baseline recompute" anti-pattern Atlas/Flyway document.

**Explicit converter is mandatory** because the content hash IS the user-edit guard:
1. Load old `InstallRecordV1` with absolute `target`.
2. Classify abs path → `(PathAnchor, relative)` via the same anchor logic future reads use. **Carry `content_hash` unchanged.**
3. Classification fail → structured warning; skip/flag the record. Never silently drop, never fabricate a hash.
4. Atomic-write V2, then delete the legacy `projects/<sha>.json` (separate, idempotent).

Lazy rebuild only as a hard fallback (every record unclassifiable) with a one-shot stderr warning `"drift baseline reset: prior content hashes replaced with current on-disk hashes"`.

### 3. Back-compat read shims and when they bite

- **Trap A — `deny_unknown_fields` + forward shim:** once V2 is persisted, an old binary hits `deny_unknown_fields` and hard-fails (correct: no silent truncation), but there is no rollback. Bump the discriminant atomically with the converter; never write V2 until conversion verified.
- **Trap B — `#[serde(default)]` on a removed field:** a `skip_serializing` ghost field silently discards V1 data on re-serialize. Use the two-struct (V1 wire / V2 in-memory) approach so `migrate_v1_record` maps old `target`/`content_hash` into `outputs` explicitly.

### 4. Atomic rewrite + advisory lock for concurrent migration

Codebase already has `atomic_write` (tempfile+rename+fsync) and `ConfigFileLock` (`fs4`, ghost-inode detection, `O_NOFOLLOW`). Sequence: acquire lock on the **new** location's owning config → read old `projects/<sha>.json` (readers never lock) → convert (preserve `content_hash`) → atomic-write new → best-effort remove old → release. Locking the new location serializes the race correctly: winner converts, loser loads the already-V2 file and skips the converter (version discriminant prevents re-convert).

### Recommendation

Two-struct (V1 wire + V2 in-memory) **explicit converter**, not lazy rebuild. `serde_repr` + version-peek probe (never `#[serde(untagged)]`). Carry `content_hash` unchanged. Lock the new location, atomic-write, best-effort-delete old. Keep `deny_unknown_fields` on V2 to catch future corruption.

### Sources

- [serde enum representations](https://serde.rs/enum-representations.html); [serde#2101](https://github.com/serde-rs/serde/issues/2101); [serde#2672](https://github.com/serde-rs/serde/issues/2672); [serde#2666](https://github.com/serde-rs/serde/issues/2666)
- [serde-repr](https://github.com/dtolnay/serde-repr); [Cargo ResolveVersion](https://doc.rust-lang.org/stable/nightly-rustc/cargo/core/resolver/enum.ResolveVersion.html); [cargo#10046](https://github.com/rust-lang/cargo/issues/10046)
- [Atlas drift detection](https://atlasgo.io/versioned/drift-detection); [untagged ruins precise errors](https://users.rust-lang.org/t/serde-untagged-enum-ruins-precise-errors/54128)
