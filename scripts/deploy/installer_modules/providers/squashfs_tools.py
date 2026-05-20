from __future__ import annotations

import os
import shutil
from pathlib import Path

from ..core import InstallContext, InstallRecord, ToolProvider


class SquashfsToolsProvider(ToolProvider):
    DEFAULT_VERSION = "4.6.1"
    ARTIFACT_PATHS = {
        "mksquashfs": "bin/mksquashfs",
        "unsquashfs": "bin/unsquashfs",
    }

    def __init__(self) -> None:
        super().__init__(name="squashfs-tools", domain="common", tools=list(self.ARTIFACT_PATHS))

    def ensure(self, ctx: InstallContext) -> Path:
        install_root = ctx.bundle_state.get("squashfs_tools_install_root")
        if isinstance(install_root, Path):
            return install_root

        version = ctx.resolve_source("squashfs_tools_version", self.DEFAULT_VERSION)
        tarball = ctx.download(
            f"https://github.com/plougher/squashfs-tools/archive/refs/tags/{version}.tar.gz",
            f"squashfs-tools-{version}.tar.gz",
        )
        build_root = ctx.config.cache_dir / "build" / f"squashfs-tools-{version}"
        install_root = ctx.config.cache_dir / "prefix" / f"squashfs-tools-{version}"

        if not ctx.config.dry_run and not (install_root / "bin" / "mksquashfs").exists():
            shutil.rmtree(build_root, ignore_errors=True)
            shutil.rmtree(install_root, ignore_errors=True)
            build_root.mkdir(parents=True, exist_ok=True)
            install_root.mkdir(parents=True, exist_ok=True)
            ctx.unpack_tarball(tarball, build_root)
            srcdir = ctx.find_single_child(build_root) / "squashfs-tools"
            ctx.run_command(["make", f"-j{os.cpu_count() or 1}"], cwd=srcdir)
            (install_root / "bin").mkdir(parents=True, exist_ok=True)
            for tool in ("mksquashfs", "unsquashfs"):
                shutil.copy2(srcdir / tool, install_root / "bin" / tool)

        ctx.bundle_state["squashfs_tools_install_root"] = install_root
        ctx.bundle_state["squashfs_tools_source"] = f"squashfs-tools:{version}"
        return install_root

    def source_label(self, ctx: InstallContext) -> str:
        source = ctx.bundle_state.get("squashfs_tools_source")
        if not isinstance(source, str):
            self.ensure(ctx)
            source = ctx.bundle_state["squashfs_tools_source"]
        return str(source)

    def install(self, requested_tool: str, ctx: InstallContext) -> InstallRecord:
        install_root = self.ensure(ctx)
        src = install_root / self.ARTIFACT_PATHS[requested_tool]
        return ctx.stage_binary(requested_tool, src, self.source_label(ctx))


PROVIDER = SquashfsToolsProvider()
