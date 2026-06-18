# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Minimal OCI registry client for the acceptance suite.

The suite pushes single-layer OCI artifacts to a local ``registry:2`` on
``localhost:5000`` over plain HTTP using only the standard library (no
extra test dependency): a tiny ``{}`` config blob typed as the OCI empty
config (``application/vnd.oci.empty.v1+json``), one uncompressed-tar layer
blob, and a manifest carrying the OCI ``artifactType``
(``application/vnd.grimoire.<kind>.v1``) plus a ``com.grimoire.kind``
annotation.

Note this is intentionally a *richer* manifest than grim's own output:
grim drops ``artifactType`` on the wire because GitLab rejects it (see
``adr_oci_empty_config_compat.md``), but ``registry:2`` accepts it, so the
harness keeps it to exercise the read path's tier-1 ``artifactType``
resolution (how new grim reads legacy / non-GitLab artifacts). grim's own
GitLab-safe output — empty config, NO ``artifactType``, kind via the
annotation — is asserted by the ``grim release`` / ``grim publish``
wire-shape tests.
"""
from __future__ import annotations

import hashlib
import json
import urllib.error
import urllib.request
from dataclasses import dataclass

REGISTRY_HOST = "localhost:5000"
REGISTRY_BASE = f"http://{REGISTRY_HOST}"

_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
_LAYER_MEDIA_TYPE = "application/vnd.grimoire.artifact.layer.v1.tar"
# The OCI empty config descriptor media type — the config every Grimoire
# manifest carries since ``adr_oci_empty_config_compat.md`` (the per-kind
# custom config type was rejected by GitLab's referenced-media-type allowlist).
_OCI_EMPTY_CONFIG_MEDIA_TYPE = "application/vnd.oci.empty.v1+json"
# The registry-agnostic kind fallback annotation key.
_KIND_ANNOTATION = "com.grimoire.kind"


def _artifact_type(kind: str) -> str:
    """The OCI ``artifactType`` for a Grimoire ``kind``."""
    return f"application/vnd.grimoire.{kind}.v1"


def _kind_from_artifact_type(artifact_type: str) -> str:
    """Parse ``application/vnd.grimoire.<kind>.v1`` back to ``<kind>``."""
    prefix, suffix = "application/vnd.grimoire.", ".v1"
    if artifact_type.startswith(prefix) and artifact_type.endswith(suffix):
        return artifact_type[len(prefix) : -len(suffix)]
    return "skill"


def _sha256(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def registry_reachable(timeout: float = 2.0) -> bool:
    """Whether the ``/v2/`` API endpoint answers."""
    try:
        with urllib.request.urlopen(f"{REGISTRY_BASE}/v2/", timeout=timeout) as resp:
            return resp.status in (200, 401)
    except (urllib.error.URLError, OSError):
        return False


def _put(url: str, data: bytes, content_type: str) -> str:
    req = urllib.request.Request(url, data=data, method="PUT")
    req.add_header("Content-Type", content_type)
    with urllib.request.urlopen(req) as resp:
        return resp.headers.get("Docker-Content-Digest", "")


def _push_blob(repo: str, data: bytes) -> str:
    """Upload ``data`` as a blob via the two-step monolithic upload."""
    digest = _sha256(data)
    start = urllib.request.Request(
        f"{REGISTRY_BASE}/v2/{repo}/blobs/uploads/", method="POST"
    )
    with urllib.request.urlopen(start) as resp:
        location = resp.headers["Location"]
    if location.startswith("/"):
        location = REGISTRY_BASE + location
    sep = "&" if "?" in location else "?"
    put_url = f"{location}{sep}digest={digest}"
    req = urllib.request.Request(put_url, data=data, method="PUT")
    req.add_header("Content-Type", "application/octet-stream")
    with urllib.request.urlopen(req):
        pass
    return digest


@dataclass(frozen=True)
class PublishedArtifact:
    """A skill or rule pushed to the test registry."""

    repo: str
    tag: str
    digest: str
    kind: str

    @property
    def fq(self) -> str:
        """Fully-qualified ``registry/repo:tag`` reference."""
        return f"{REGISTRY_HOST}/{self.repo}:{self.tag}"

    @property
    def pinned(self) -> str:
        """Fully-qualified ``registry/repo@digest`` reference."""
        return f"{REGISTRY_HOST}/{self.repo}@{self.digest}"


def push_artifact(
    repo: str,
    tag: str,
    tar_bytes: bytes,
    kind: str,
    annotations: dict[str, str] | None = None,
) -> PublishedArtifact:
    """Push a single-layer OCI artifact and tag it.

    ``tar_bytes`` is the uncompressed artifact tar the materializer
    expects. The kind rides on the OCI ``artifactType`` (per ``kind``) and
    is mirrored into the ``com.grimoire.kind`` annotation; the config
    descriptor is the OCI empty type. Any extra ``annotations`` are merged
    in (and may override ``com.grimoire.kind``). Returns the published
    reference incl. the manifest digest, so callers can assert ``@sha256``
    pins.
    """
    config_blob = b"{}"
    config_digest = _push_blob(repo, config_blob)
    layer_digest = _push_blob(repo, tar_bytes)

    merged_annotations = {_KIND_ANNOTATION: kind}
    if annotations:
        merged_annotations.update(annotations)

    manifest = {
        "schemaVersion": 2,
        "mediaType": _MANIFEST_MEDIA_TYPE,
        "artifactType": _artifact_type(kind),
        "config": {
            "mediaType": _OCI_EMPTY_CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": len(config_blob),
        },
        "layers": [
            {
                "mediaType": _LAYER_MEDIA_TYPE,
                "digest": layer_digest,
                "size": len(tar_bytes),
            }
        ],
        "annotations": merged_annotations,
    }
    manifest_bytes = json.dumps(manifest).encode()
    manifest_digest = _sha256(manifest_bytes)
    _put(
        f"{REGISTRY_BASE}/v2/{repo}/manifests/{tag}",
        manifest_bytes,
        _MANIFEST_MEDIA_TYPE,
    )
    return PublishedArtifact(
        repo=repo, tag=tag, digest=manifest_digest, kind=kind
    )


def tag_digest(repo: str, tag: str) -> str:
    """Return the manifest digest a ``tag`` currently resolves to.

    Issues a manifest ``GET`` (``HEAD`` is not universally enabled on
    ``registry:2``) and reads the ``Docker-Content-Digest`` header — the
    authoritative answer to "what does this floating tag point at right
    now", used to assert a rolling release actually moved the cascade.
    """
    req = urllib.request.Request(
        f"{REGISTRY_BASE}/v2/{repo}/manifests/{tag}",
        headers={"Accept": _MANIFEST_MEDIA_TYPE},
    )
    with urllib.request.urlopen(req) as resp:
        header = resp.headers.get("Docker-Content-Digest")
        if header:
            return header
        return _sha256(resp.read())


def fetch_manifest(repo: str, tag: str) -> dict:
    """Fetch and JSON-decode the raw manifest a ``tag`` resolves to.

    Lets a test assert the on-the-wire contract directly — `artifactType`,
    the config descriptor's media type, the absence of legacy annotations.
    """
    req = urllib.request.Request(
        f"{REGISTRY_BASE}/v2/{repo}/manifests/{tag}",
        headers={"Accept": _MANIFEST_MEDIA_TYPE},
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def retag(repo: str, tag: str, target_digest: str) -> PublishedArtifact:
    """Re-point ``tag`` at an existing manifest ``target_digest``.

    Models a rolling release: the floating tag is moved to a manifest
    that is already in the registry. Returns the new published ref.
    """
    with urllib.request.urlopen(
        urllib.request.Request(
            f"{REGISTRY_BASE}/v2/{repo}/manifests/{target_digest}",
            headers={"Accept": _MANIFEST_MEDIA_TYPE},
        )
    ) as resp:
        manifest_bytes = resp.read()
    _put(
        f"{REGISTRY_BASE}/v2/{repo}/manifests/{tag}",
        manifest_bytes,
        _MANIFEST_MEDIA_TYPE,
    )
    manifest = json.loads(manifest_bytes)
    kind = _kind_from_artifact_type(manifest.get("artifactType", ""))
    return PublishedArtifact(
        repo=repo, tag=tag, digest=_sha256(manifest_bytes), kind=kind
    )
