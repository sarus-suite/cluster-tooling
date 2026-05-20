from __future__ import annotations

import shlex
from pathlib import Path
from typing import Dict, List

from ..core import InstallContext, InstallRecord, InstallerError, ToolProvider


class PodmanStaticProvider(ToolProvider):
    DEFAULT_VERSION = "v5.8.2"
    DEFAULT_URL_TEMPLATE = (
        "https://github.com/mgoltzsche/podman-static/releases/download/{version}/"
        "podman-linux-{arch}.tar.gz"
    )
    ARTIFACT_PATHS: Dict[str, List[str]] = {
        "podman": ["usr/local/bin/podman", "usr/bin/podman"],
        "conmon": [
            "usr/local/libexec/podman/conmon",
            "usr/local/lib/podman/conmon",
            "usr/libexec/podman/conmon",
            "usr/lib/podman/conmon",
            "usr/bin/conmon",
            "usr/local/bin/conmon",
        ],
        "crun": ["usr/local/bin/crun", "usr/bin/crun"],
        "fuse-overlayfs": ["usr/local/bin/fuse-overlayfs", "usr/bin/fuse-overlayfs"],
        "fusermount3": ["usr/local/bin/fusermount3", "usr/bin/fusermount3"],
        "pasta": ["usr/local/bin/pasta", "usr/bin/pasta"],
    }

    def __init__(self) -> None:
        super().__init__(
            name="podman-static",
            domain="common",
            tools=list(self.ARTIFACT_PATHS),
        )

    def ensure(self, ctx: InstallContext) -> Path:
        bundle_root = ctx.bundle_state.get("podman_static_bundle_root")
        if isinstance(bundle_root, Path):
            return bundle_root

        version = ctx.resolve_source("podman_static_version", self.DEFAULT_VERSION)
        url = ctx.resolve_source(
            "podman_static_url",
            self.DEFAULT_URL_TEMPLATE.format(version=version, arch=ctx.arch),
        )
        download_path = ctx.download(url, "podman-static.tar.gz")
        unpack_root = ctx.config.cache_dir / "unpacked" / "podman-static"

        if not ctx.config.dry_run:
            ctx.unpack_tarball(download_path, unpack_root)
            bundle_root = ctx.find_single_child(unpack_root)
        else:
            bundle_root = unpack_root / "podman-static"

        ctx.bundle_state["podman_static_bundle_root"] = bundle_root
        ctx.bundle_state["podman_static_source"] = f"podman-static:{url}"
        return bundle_root

    def source_label(self, ctx: InstallContext) -> str:
        source = ctx.bundle_state.get("podman_static_source")
        if not isinstance(source, str):
            self.ensure(ctx)
            source = ctx.bundle_state["podman_static_source"]
        return str(source)

    def support_root(self, ctx: InstallContext) -> Path:
        return ctx.config.support_root / "podman-static"

    def helper_dirs(self, ctx: InstallContext) -> List[Path]:
        support_root = self.support_root(ctx)
        return [
            support_root / "usr/local/libexec/podman",
            support_root / "usr/local/lib/podman",
            support_root / "usr/libexec/podman",
            support_root / "usr/lib/podman",
            support_root / "usr/local/bin",
            support_root / "usr/bin",
        ]

    def install_support_once(self, ctx: InstallContext) -> None:
        bundle_root = self.ensure(ctx)
        source = self.source_label(ctx)
        for rel in ("usr", "etc"):
            src = bundle_root / rel
            if src.exists() or ctx.config.dry_run:
                record = ctx.stage_tree_once(
                    state_key=f"podman-static-support:{rel}",
                    name=f"podman-static-{rel}",
                    src=src,
                    relative_root=Path("podman-static") / rel,
                    source=source,
                    note="Installed under a private support tree instead of the host root.",
                )
                ctx.remember_support_record(record)

        config_record = ctx.stage_file(
            name="podman-static-containers-conf",
            content=self._render_containers_conf(ctx),
            relative_path=Path("podman-static/config/containers.conf"),
            source=source,
            note="Generated config that points Podman at the private helper-binary tree.",
        )
        ctx.remember_support_record(config_record)

    def _render_containers_conf(self, ctx: InstallContext) -> str:
        helper_dirs_list = self.helper_dirs(ctx)
        helper_dirs = ", ".join(f'"{path}"' for path in helper_dirs_list)
        conmon_path = ", ".join(f'"{path / "conmon"}"' for path in helper_dirs_list)
        return (
            "[engine]\n"
            f"helper_binaries_dir = [{helper_dirs}]\n"
            f"conmon_path = [{conmon_path}]\n"
        )

    def render_wrapper(self, ctx: InstallContext, tool_relpath: Path) -> str:
        support_root = self.support_root(ctx)
        helper_path = ":".join(str(path) for path in self.helper_dirs(ctx))
        containers_conf = support_root / "config" / "containers.conf"
        quoted_tool = shlex.quote(str(support_root / tool_relpath))
        quoted_conf = shlex.quote(str(containers_conf))
        quoted_helper_path = shlex.quote(helper_path)
        return "\n".join(
            [
                "#!/usr/bin/env sh",
                "set -eu",
                f'export PATH={quoted_helper_path}:"$PATH"',
                f'export CONTAINERS_CONF={quoted_conf}',
                f'exec {quoted_tool} "$@"',
                "",
            ]
        )

    def install(self, requested_tool: str, ctx: InstallContext) -> InstallRecord:
        if requested_tool not in self.ARTIFACT_PATHS:
            raise InstallerError(f"provider '{self.name}' does not manage tool '{requested_tool}'")

        bundle_root = self.ensure(ctx)
        self.install_support_once(ctx)
        src = self._resolve_artifact_path(ctx, bundle_root, requested_tool)
        if requested_tool == "podman":
            tool_relpath = src.relative_to(bundle_root)
            return ctx.stage_script(
                requested_tool,
                self.render_wrapper(ctx, tool_relpath),
                self.source_label(ctx),
                "Wrapper script that runs Podman against the private support tree.",
            )
        return ctx.stage_binary(requested_tool, src, self.source_label(ctx))

    def _resolve_artifact_path(
        self, ctx: InstallContext, bundle_root: Path, requested_tool: str
    ) -> Path:
        artifact_paths = [Path(candidate) for candidate in self.ARTIFACT_PATHS[requested_tool]]
        if ctx.config.dry_run:
            return bundle_root / artifact_paths[0]

        for artifact_path in artifact_paths:
            candidate = bundle_root / artifact_path
            if candidate.exists():
                return candidate

        joined = ", ".join(str(path) for path in artifact_paths)
        raise InstallerError(
            f"could not locate artifact for {requested_tool} in bundle; tried: {joined}"
        )


PROVIDER = PodmanStaticProvider()
