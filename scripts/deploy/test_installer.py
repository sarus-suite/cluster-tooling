from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
import sys
import types

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.modules.setdefault("yaml", types.SimpleNamespace(safe_load=lambda _: {}))

from installer_modules.core import (
    HostToolsInstaller,
    InstallerConfig,
    InstallerError,
    discover_providers,
)
from installer_modules.providers.podman_static import PROVIDER as PODMAN_PROVIDER


def make_config(mode: str, dry_run: bool = True) -> InstallerConfig:
    temp_root = Path(tempfile.mkdtemp(prefix="deploy-installer-test-"))
    return InstallerConfig(
        mode=mode,
        manifest_path=temp_root / f"{mode}.yaml",
        cache_dir=temp_root / "cache",
        install_prefix=temp_root / "bin",
        support_root=temp_root / "support",
        state_root=temp_root / "state",
        stage_root=temp_root / "stage",
        dry_run=dry_run,
    )


class ProviderDiscoveryTests(unittest.TestCase):
    def test_discovered_tool_map_covers_current_components(self) -> None:
        _, tool_map = discover_providers()

        self.assertEqual(
            {
                "podman",
                "conmon",
                "crun",
                "fuse-overlayfs",
                "fusermount3",
                "pasta",
                "squashfuse",
                "squashfuse_ll",
                "mksquashfs",
                "unsquashfs",
                "parallax",
                "parallax-mount-program",
            },
            set(tool_map),
        )


class PodmanStaticProviderTests(unittest.TestCase):
    def test_common_bundle_uses_pinned_release_url(self) -> None:
        manifest = {
            "sources": {"podman_static_version": "v5.8.2"},
            "profiles": {"runtime": {"enabled": True, "tools": ["podman"]}},
        }
        ctx = HostToolsInstaller(make_config("common"), manifest).ctx

        PODMAN_PROVIDER.ensure(ctx)

        source = ctx.bundle_state["podman_static_source"]
        self.assertIsInstance(source, str)
        self.assertIn("/releases/download/v5.8.2/", source)
        self.assertNotIn("/releases/latest/", source)

    def test_common_bundle_allows_manifest_url_override(self) -> None:
        manifest = {
            "sources": {"podman_static_url": "https://example.invalid/podman.tar.gz"},
            "profiles": {"runtime": {"enabled": True, "tools": ["podman"]}},
        }
        ctx = HostToolsInstaller(make_config("common"), manifest).ctx

        PODMAN_PROVIDER.ensure(ctx)

        source = ctx.bundle_state["podman_static_source"]
        self.assertEqual(source, "podman-static:https://example.invalid/podman.tar.gz")

    def test_common_bundle_tracks_private_support_tree(self) -> None:
        manifest = {
            "sources": {"podman_static_version": "v5.8.2"},
            "profiles": {"runtime": {"enabled": True, "tools": ["podman"]}},
        }
        ctx = HostToolsInstaller(make_config("common"), manifest).ctx

        PODMAN_PROVIDER.install_support_once(ctx)

        self.assertEqual(len(ctx.support_records), 3)
        installed_roots = {record.installed_files[0] for record in ctx.support_records}
        self.assertIn(str(ctx.config.support_root / "podman-static" / "usr"), installed_roots)
        self.assertIn(str(ctx.config.support_root / "podman-static" / "etc"), installed_roots)
        self.assertIn(
            str(ctx.config.support_root / "podman-static" / "config" / "containers.conf"),
            installed_roots,
        )

    def test_common_bundle_prefers_usr_local_lib_podman_and_usr_bin_fallbacks(self) -> None:
        manifest = {"profiles": {"runtime": {"enabled": True, "tools": ["podman", "conmon"]}}}
        ctx = HostToolsInstaller(make_config("common", dry_run=False), manifest).ctx
        bundle_root = ctx.config.cache_dir / "fake-podman-static"
        (bundle_root / "usr/bin").mkdir(parents=True, exist_ok=True)
        (bundle_root / "usr/local/lib/podman").mkdir(parents=True, exist_ok=True)
        (bundle_root / "usr/bin/podman").write_text("", encoding="utf-8")
        (bundle_root / "usr/local/lib/podman/conmon").write_text("", encoding="utf-8")
        ctx.bundle_state["podman_static_bundle_root"] = bundle_root
        ctx.bundle_state["podman_static_source"] = "podman-static:test"

        podman_record = PODMAN_PROVIDER.install("podman", ctx)
        conmon_record = PODMAN_PROVIDER.install("conmon", ctx)

        self.assertEqual(podman_record.tool, "podman")
        self.assertEqual(conmon_record.tool, "conmon")
        containers_conf = (
            ctx.config.support_root / "podman-static" / "config" / "containers.conf"
        ).read_text(encoding="utf-8")
        self.assertIn("/usr/local/lib/podman", containers_conf)
        self.assertIn("/usr/bin/conmon", containers_conf)


class HostToolsInstallerValidationTests(unittest.TestCase):
    def test_common_manifest_allows_sources(self) -> None:
        manifest = {
            "sources": {"podman_static_version": "v5.8.2"},
            "profiles": {"runtime": {"enabled": True, "tools": ["podman"]}},
        }
        installer = HostToolsInstaller(make_config("common"), manifest)

        installer._validate_manifest()

    def test_manifest_sources_must_be_a_mapping(self) -> None:
        manifest = {
            "sources": ["podman_static_version=v5.8.2"],
            "profiles": {"runtime": {"enabled": True, "tools": ["podman"]}},
        }
        installer = HostToolsInstaller(make_config("common"), manifest)

        with self.assertRaises(InstallerError):
            installer._validate_manifest()

    def test_run_command_reports_missing_build_tool_cleanly(self) -> None:
        installer = HostToolsInstaller(make_config("common", dry_run=False), {"profiles": {}})

        with self.assertRaises(InstallerError) as ctx:
            installer.ctx.run_command(["definitely-missing-build-tool"], Path("/"))

        self.assertIn("required build command 'definitely-missing-build-tool'", str(ctx.exception))


class HostToolsInstallerManifestTests(unittest.TestCase):
    def test_install_manifest_includes_support_artifacts(self) -> None:
        config = make_config("common", dry_run=False)
        installer = HostToolsInstaller(config, {"profiles": {}})
        installer.ctx.records.append(
            installer.ctx.stage_script("podman", "#!/usr/bin/env sh\n", "podman-static:test")
        )
        installer.ctx.support_records.append(
            installer.ctx.stage_file(
                "podman-static-containers-conf",
                "[engine]\n",
                Path("podman-static/config/containers.conf"),
                "podman-static:test",
            )
        )

        installer._prepare_dirs()
        manifest_path = installer._write_install_manifest()
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))

        self.assertIn("support_artifacts", payload)
        self.assertEqual(payload["tools"][0]["tool"], "podman")
        self.assertEqual(payload["support_artifacts"][0]["name"], "podman-static-containers-conf")


if __name__ == "__main__":
    unittest.main()
