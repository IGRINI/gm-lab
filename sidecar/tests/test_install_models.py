from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock


SIDECAR_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SIDECAR_DIR))

import install_models  # noqa: E402


class InstallModelsTests(unittest.TestCase):
    def test_manifest_profiles_are_cumulative(self) -> None:
        manifest = install_models.load_manifest(SIDECAR_DIR / "models.json")
        ids = lambda profile: {
            item["id"] for item in install_models.selected_components(manifest, profile)
        }

        self.assertEqual(ids("Minimal"), set())
        self.assertTrue({"embedder", "reranker"}.issubset(ids("Rag")))
        self.assertTrue(ids("Rag").issubset(ids("Voice")))
        self.assertTrue(ids("Rag").issubset(ids("Images")))
        self.assertEqual(ids("Full"), ids("Voice") | ids("Images"))

    def test_destination_cannot_escape_inference_home(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(install_models.InstallError):
                install_models.safe_destination(root, "../outside")

    def test_snapshot_verification_and_marker_gate(self) -> None:
        payload = b"model"
        component = {
            "id": "fixture",
            "kind": "hf_snapshot",
            "destination": "models/fixture",
            "revision": "immutable",
            "files": ["config.json", "model.safetensors"],
            "checksums": {"model.safetensors": hashlib.sha256(payload).hexdigest()},
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / component["destination"]
            destination.mkdir(parents=True)
            (destination / "config.json").write_text("{}", encoding="utf-8")
            (destination / "model.safetensors").write_bytes(payload)

            install_models.verify_component(component, root)
            install_models.write_marker(component, destination)
            self.assertTrue(install_models.marker_matches(component, destination))

            (destination / "config.json").unlink()
            self.assertFalse(install_models.marker_matches(component, destination))

    def test_marker_covers_complete_component_manifest(self) -> None:
        payload = b"model"
        component = {
            "id": "fixture",
            "kind": "http_file",
            "destination": "models/fixture.bin",
            "url": "https://example.invalid/fixture.bin",
            "revision": "immutable",
            "sha256": hashlib.sha256(payload).hexdigest(),
            "estimated_bytes": len(payload),
            "license": "test-license",
        }
        mutations = {
            "url": "https://example.invalid/replacement.bin",
            "sha256": hashlib.sha256(b"other").hexdigest(),
            "revision": "different-revision",
            "license": "changed-manifest-field",
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / component["destination"]
            destination.parent.mkdir(parents=True)
            destination.write_bytes(payload)
            install_models.write_marker(component, destination)

            self.assertTrue(install_models.marker_matches(component, destination))
            for field, value in mutations.items():
                with self.subTest(field=field):
                    changed = copy.deepcopy(component)
                    changed[field] = value
                    self.assertFalse(install_models.marker_matches(changed, destination))

    def test_repeat_install_repairs_same_size_corruption(self) -> None:
        expected = b"healthy"
        component = {
            "id": "fixture",
            "kind": "http_file",
            "destination": "models/fixture.bin",
            "url": "https://example.invalid/fixture.bin",
            "sha256": hashlib.sha256(expected).hexdigest(),
            "estimated_bytes": len(expected),
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / component["destination"]
            destination.parent.mkdir(parents=True)
            destination.write_bytes(expected)
            install_models.write_marker(component, destination)
            destination.write_bytes(b"damaged")

            def download(_url: str, target: Path) -> None:
                target.write_bytes(expected)

            with mock.patch.object(
                install_models, "download_http_file", side_effect=download
            ) as downloader:
                install_models.install_component(component, root, token=None)

            downloader.assert_called_once_with(component["url"], destination)
            self.assertEqual(destination.read_bytes(), expected)
            self.assertTrue(install_models.marker_matches(component, destination))

    def test_repeat_install_repairs_missing_snapshot_artifact(self) -> None:
        model = b"model"
        component = {
            "id": "fixture",
            "kind": "hf_snapshot",
            "destination": "models/fixture",
            "repo": "example/fixture",
            "revision": "immutable",
            "files": ["config.json", "model.safetensors"],
            "checksums": {"model.safetensors": hashlib.sha256(model).hexdigest()},
        }
        calls: list[dict[str, object]] = []

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / component["destination"]
            destination.mkdir(parents=True)
            (destination / "config.json").write_text("{}", encoding="utf-8")
            (destination / "model.safetensors").write_bytes(model)
            install_models.write_marker(component, destination)
            (destination / "config.json").unlink()

            hub = types.ModuleType("huggingface_hub")

            def snapshot_download(**kwargs: object) -> str:
                calls.append(kwargs)
                (destination / "config.json").write_text("{}", encoding="utf-8")
                (destination / "model.safetensors").write_bytes(model)
                return str(destination)

            hub.snapshot_download = snapshot_download  # type: ignore[attr-defined]
            with mock.patch.dict(sys.modules, {"huggingface_hub": hub}):
                install_models.install_component(component, root, token=None)

            self.assertEqual(len(calls), 1)
            self.assertIs(calls[0]["force_download"], True)
            self.assertTrue((destination / "config.json").is_file())
            self.assertTrue(install_models.marker_matches(component, destination))

    def test_restricted_profile_requires_explicit_acceptance(self) -> None:
        manifest = {
            "schema_version": 1,
            "components": [
                {
                    "id": "restricted",
                    "kind": "http_file",
                    "profiles": ["Rag"],
                    "destination": "restricted.bin",
                    "url": "https://example.invalid/restricted.bin",
                    "restricted": True,
                }
            ],
        }
        with tempfile.TemporaryDirectory() as temporary:
            manifest_path = Path(temporary) / "models.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            code = install_models.main(
                [
                    "--manifest",
                    str(manifest_path),
                    "--home",
                    str(Path(temporary) / "inference"),
                    "--profile",
                    "Rag",
                    "--plan",
                ]
            )
            self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
