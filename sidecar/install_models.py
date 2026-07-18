"""Install and verify TaleShift model artifacts from an immutable manifest.

The installer is intentionally independent from application settings. It only
writes below ``--home``, resumes interrupted downloads, verifies large files,
and creates readiness markers after a component is complete.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


PROFILES = ("Minimal", "Rag", "Voice", "Images", "Full")
MARKER_NAME = ".gml-model.json"
MARKER_SCHEMA_VERSION = 2


class InstallError(RuntimeError):
    """Expected installation failure with a user-facing message."""


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"cannot read model manifest {path}: {exc}") from exc
    if manifest.get("schema_version") != 1 or not isinstance(manifest.get("components"), list):
        raise InstallError(f"unsupported model manifest: {path}")
    return manifest


def selected_components(manifest: dict[str, Any], profile: str) -> list[dict[str, Any]]:
    if profile == "Minimal":
        return []
    return [component for component in manifest["components"] if profile in component.get("profiles", [])]


def safe_destination(home: Path, relative: str) -> Path:
    root = home.resolve()
    destination = (root / relative).resolve()
    try:
        destination.relative_to(root)
    except ValueError as exc:
        raise InstallError(f"manifest destination escapes inference home: {relative}") from exc
    return destination


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(8 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def component_manifest_sha256(component: dict[str, Any]) -> str:
    """Return a stable fingerprint of the complete component manifest entry."""
    encoded = json.dumps(
        component,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def snapshot_artifact_path(destination: Path, relative: str) -> Path:
    root = destination.resolve()
    artifact = (root / relative).resolve()
    try:
        artifact.relative_to(root)
    except ValueError as exc:
        raise InstallError(f"snapshot artifact escapes its destination: {relative}") from exc
    return artifact


def component_artifacts(
    component: dict[str, Any], destination: Path
) -> list[tuple[str, Path]]:
    if component["kind"] == "hf_snapshot":
        return [
            (relative, snapshot_artifact_path(destination, relative))
            for relative in component.get("files", [])
        ]
    return [(".", destination)]


def artifact_inventory(component: dict[str, Any], destination: Path) -> dict[str, Any]:
    inventory: dict[str, Any] = {}
    for relative, artifact in component_artifacts(component, destination):
        if not artifact.is_file():
            raise InstallError(f"{component['id']}: missing {relative}")
        inventory[relative] = {
            "size": artifact.stat().st_size,
            "sha256": sha256(artifact),
        }
    return inventory


def marker_path(component: dict[str, Any], destination: Path) -> Path:
    if component["kind"] == "hf_snapshot":
        return destination / MARKER_NAME
    return destination.with_name(f"{destination.name}.gml-model.json")


def marker_matches(component: dict[str, Any], destination: Path) -> bool:
    marker = marker_path(component, destination)
    try:
        body = json.loads(marker.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    if (
        body.get("schema_version") != MARKER_SCHEMA_VERSION
        or body.get("component") != component["id"]
        or body.get("manifest_sha256") != component_manifest_sha256(component)
        or not destination.exists()
    ):
        return False

    stored_inventory = body.get("artifacts")
    if not isinstance(stored_inventory, dict):
        return False
    expected_paths = {relative for relative, _ in component_artifacts(component, destination)}
    if set(stored_inventory) != expected_paths:
        return False

    expected_hashes = component.get("checksums", {})
    if component["kind"] != "hf_snapshot" and component.get("sha256"):
        expected_hashes = {".": component["sha256"]}
    for relative, artifact in component_artifacts(component, destination):
        record = stored_inventory.get(relative)
        if not isinstance(record, dict) or not artifact.is_file():
            return False
        size = artifact.stat().st_size
        if record.get("size") != size:
            return False
        actual_hash = sha256(artifact).lower()
        if record.get("sha256", "").lower() != actual_hash:
            return False
        expected_hash = expected_hashes.get(relative)
        if expected_hash and expected_hash.lower() != actual_hash:
            return False
    return True


def verify_component(component: dict[str, Any], home: Path) -> None:
    destination = safe_destination(home, component["destination"])
    kind = component["kind"]
    if kind == "hf_snapshot":
        if not destination.is_dir():
            raise InstallError(f"{component['id']}: missing directory {destination}")
        for relative in component.get("files", []):
            if not snapshot_artifact_path(destination, relative).is_file():
                raise InstallError(f"{component['id']}: missing {relative}")
        for relative, expected in component.get("checksums", {}).items():
            actual = sha256(snapshot_artifact_path(destination, relative))
            if actual.lower() != expected.lower():
                raise InstallError(f"{component['id']}: SHA-256 mismatch for {relative}")
        return

    if not destination.is_file():
        raise InstallError(f"{component['id']}: missing file {destination}")
    expected_size = component.get("estimated_bytes")
    if expected_size and destination.stat().st_size != int(expected_size):
        raise InstallError(
            f"{component['id']}: size mismatch ({destination.stat().st_size} != {expected_size})"
        )
    expected_hash = component.get("sha256")
    if expected_hash and sha256(destination).lower() != expected_hash.lower():
        raise InstallError(f"{component['id']}: SHA-256 mismatch")


def write_marker(component: dict[str, Any], destination: Path) -> None:
    marker = marker_path(component, destination)
    marker.parent.mkdir(parents=True, exist_ok=True)
    body = {
        "schema_version": MARKER_SCHEMA_VERSION,
        "component": component["id"],
        "manifest_sha256": component_manifest_sha256(component),
        "repo": component.get("repo"),
        "revision": component.get("revision"),
        "url": component.get("url"),
        "artifacts": artifact_inventory(component, destination),
        "installed_at": int(time.time()),
    }
    temporary = marker.with_name(f"{marker.name}.tmp")
    temporary.write_text(json.dumps(body, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, marker)


def install_snapshot(
    component: dict[str, Any],
    home: Path,
    token: str | None,
    force_download: bool = False,
) -> None:
    try:
        from huggingface_hub import snapshot_download
    except ImportError as exc:
        raise InstallError("huggingface-hub is not installed in the setup environment") from exc

    destination = safe_destination(home, component["destination"])
    destination.mkdir(parents=True, exist_ok=True)
    print(f"[models] downloading {component['id']} ({component['repo']}@{component['revision']})")
    def download(force: bool) -> None:
        snapshot_download(
            repo_id=component["repo"],
            revision=component["revision"],
            allow_patterns=component.get("files"),
            local_dir=str(destination),
            token=token,
            force_download=force,
        )

    download(force_download)
    try:
        verify_component(component, home)
    except InstallError:
        if force_download:
            raise
        print(f"[models] {component['id']}: retrying a clean snapshot download")
        download(True)
        verify_component(component, home)
    write_marker(component, destination)


def install_hf_file(
    component: dict[str, Any],
    home: Path,
    token: str | None,
    force_download: bool = False,
) -> None:
    try:
        from huggingface_hub import hf_hub_download
    except ImportError as exc:
        raise InstallError("huggingface-hub is not installed in the setup environment") from exc

    destination = safe_destination(home, component["destination"])
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = safe_destination(home, f".downloads/{component['id']}")
    staging.mkdir(parents=True, exist_ok=True)
    print(f"[models] downloading {component['id']} ({component['repo']}@{component['revision']})")
    downloaded = Path(
        hf_hub_download(
            repo_id=component["repo"],
            filename=component["source"],
            revision=component["revision"],
            local_dir=str(staging),
            token=token,
            force_download=force_download,
        )
    )
    if sha256(downloaded).lower() != component["sha256"].lower():
        raise InstallError(f"{component['id']}: SHA-256 mismatch after download")
    temporary = destination.with_name(f"{destination.name}.tmp")
    os.replace(downloaded, temporary)
    os.replace(temporary, destination)
    shutil.rmtree(staging, ignore_errors=True)
    verify_component(component, home)
    write_marker(component, destination)


def download_http_file(url: str, destination: Path) -> None:
    partial = destination.with_name(f"{destination.name}.part")
    existing = partial.stat().st_size if partial.exists() else 0
    headers = {"User-Agent": "taleshift-setup/1"}
    if existing:
        headers["Range"] = f"bytes={existing}-"
    request = urllib.request.Request(url, headers=headers)
    try:
        response = urllib.request.urlopen(request, timeout=60)
    except urllib.error.HTTPError as exc:
        if exc.code == 416 and existing:
            os.replace(partial, destination)
            return
        raise InstallError(f"download failed ({exc.code}) for {url}") from exc
    mode = "ab" if existing and getattr(response, "status", None) == 206 else "wb"
    with response, partial.open(mode) as stream:
        shutil.copyfileobj(response, stream, length=1024 * 1024)
    os.replace(partial, destination)


def install_http_file(component: dict[str, Any], home: Path) -> None:
    destination = safe_destination(home, component["destination"])
    destination.parent.mkdir(parents=True, exist_ok=True)
    print(f"[models] downloading {component['id']} ({component['url']})")
    download_http_file(component["url"], destination)
    try:
        verify_component(component, home)
    except InstallError:
        partial = destination.with_name(f"{destination.name}.part")
        partial.unlink(missing_ok=True)
        print(f"[models] {component['id']}: retrying a clean download")
        download_http_file(component["url"], destination)
        verify_component(component, home)
    write_marker(component, destination)


def install_component(component: dict[str, Any], home: Path, token: str | None) -> None:
    destination = safe_destination(home, component["destination"])
    if marker_matches(component, destination):
        print(f"[models] {component['id']}: already installed")
        return
    marker = marker_path(component, destination)
    repair_existing_install = marker.exists()
    marker.unlink(missing_ok=True)
    kind = component["kind"]
    if kind == "hf_snapshot":
        install_snapshot(component, home, token, force_download=repair_existing_install)
    elif kind == "hf_file":
        install_hf_file(component, home, token, force_download=repair_existing_install)
    elif kind == "http_file":
        install_http_file(component, home)
    else:
        raise InstallError(f"{component['id']}: unsupported kind {kind}")
    print(f"[models] {component['id']}: ready")


def print_plan(components: list[dict[str, Any]], home: Path) -> None:
    total = sum(int(component.get("estimated_bytes", 0)) for component in components)
    print(f"Inference home: {home}")
    print(f"Model download: {total / (1024 ** 3):.2f} GiB")
    if not components:
        print("Components: none")
        return
    print("Components:")
    for component in components:
        restriction = " [restricted license]" if component.get("restricted") else ""
        print(f"  - {component['id']}: {component.get('license', 'unknown')}{restriction}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Install pinned TaleShift inference models")
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("models.json"))
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--profile", choices=PROFILES, required=True)
    parser.add_argument("--accept-restricted", action="store_true")
    parser.add_argument("--plan", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    try:
        manifest = load_manifest(args.manifest.resolve())
        home = args.home.resolve()
        components = selected_components(manifest, args.profile)
        restricted = [component for component in components if component.get("restricted")]
        if restricted and not args.accept_restricted:
            names = ", ".join(component["id"] for component in restricted)
            raise InstallError(
                f"profile contains restricted or unverified model licenses ({names}); "
                "review THIRD_PARTY_NOTICES.md and pass --accept-restricted"
            )
        print_plan(components, home)
        if args.plan:
            return 0
        home.mkdir(parents=True, exist_ok=True)
        if args.verify_only:
            for component in components:
                verify_component(component, home)
                destination = safe_destination(home, component["destination"])
                if not marker_matches(component, destination):
                    raise InstallError(
                        f"{component['id']}: readiness marker is missing, outdated, or invalid"
                    )
                print(f"[verify] {component['id']}: OK")
            return 0

        token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
        for component in components:
            install_component(component, home, token)
        return 0
    except InstallError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("Interrupted; rerun the same command to resume.", file=sys.stderr)
        return 130
    except Exception as exc:
        print(f"ERROR: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
