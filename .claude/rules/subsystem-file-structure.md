---
paths:
  - src/**
---

# File Structure Subsystem

How Grimoire lays out its on-disk data: where downloaded artifacts, the
local index, and install links live under the data root.

> **Status: provisional.** The storage layer is not implemented yet (only
> `src/main.rs` exists). This file records intent and design constraints,
> not shipped behavior. Do not invent concrete storage internals — fill
> this in as the real design lands.

## Design Rationale (intended)

- **Single data root.** All Grimoire state lives under one directory
  (default `~/.grimoire`, overridable via `GRIM_HOME`). Keeping everything
  under one root makes atomic rename / hardlink operations possible because
  source and destination stay on one filesystem.
- **Content-addressed storage.** Downloaded artifacts are addressed by
  content hash so identical content is stored once and is immutable.
- **Mutable namespace on top.** Human-facing names (tags, "installed"
  links) are a thin mutable layer pointing at immutable content.
- **Cross-device safety.** Operations that rely on same-filesystem
  atomic rename must validate the data root sits on a single volume;
  cross-device hardlink/rename fails and must be handled explicitly.

## Constraints

- Never assume a path operation crosses filesystems silently — check first.
- Treat the content store as append-only / immutable; mutate only the
  name → content mapping.
- Concurrent processes must coordinate via advisory file locks for any
  read-modify-write on shared metadata.

## Cross-References

- `arch-principles.md` — overall architecture and utility discipline
- `quality-security.md` — path traversal / symlink-escape guards
