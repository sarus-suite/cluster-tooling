from __future__ import annotations

import os
import shutil
from pathlib import Path

from ..core import InstallContext, InstallRecord, ToolProvider


class SquashfuseProvider(ToolProvider):
    DEFAULT_VERSION = "0.6.1"
    ARTIFACT_PATHS = {
        "squashfuse": "bin/squashfuse",
        "squashfuse_ll": "bin/squashfuse_ll",
    }

    def __init__(self) -> None:
        super().__init__(name="squashfuse", domain="common", tools=list(self.ARTIFACT_PATHS))

    def ensure(self, ctx: InstallContext) -> Path:
        install_root = ctx.bundle_state.get("squashfuse_install_root")
        if isinstance(install_root, Path):
            return install_root

        version = ctx.resolve_source("squashfuse_version", self.DEFAULT_VERSION)
        tarball = ctx.download(
            f"https://github.com/vasi/squashfuse/releases/download/{version}/squashfuse-{version}.tar.gz",
            f"squashfuse-{version}.tar.gz",
        )
        build_root = ctx.config.cache_dir / "build" / f"squashfuse-{version}"
        install_root = ctx.config.cache_dir / "prefix" / f"squashfuse-{version}"

        if not ctx.config.dry_run and not (install_root / "bin" / "squashfuse").exists():
            shutil.rmtree(build_root, ignore_errors=True)
            shutil.rmtree(install_root, ignore_errors=True)
            build_root.mkdir(parents=True, exist_ok=True)
            ctx.unpack_tarball(tarball, build_root)
            srcdir = ctx.find_single_child(build_root)
            ctx.run_command(["./configure", f"--prefix={install_root}"], cwd=srcdir)
            ctx.run_command(["make", f"-j{os.cpu_count() or 1}"], cwd=srcdir)
            ctx.run_command(["make", "install"], cwd=srcdir)

        ctx.bundle_state["squashfuse_install_root"] = install_root
        ctx.bundle_state["squashfuse_source"] = f"squashfuse:{version}"
        return install_root

    def source_label(self, ctx: InstallContext) -> str:
        source = ctx.bundle_state.get("squashfuse_source")
        if not isinstance(source, str):
            self.ensure(ctx)
            source = ctx.bundle_state["squashfuse_source"]
        return str(source)

    def install(self, requested_tool: str, ctx: InstallContext) -> InstallRecord:
        install_root = self.ensure(ctx)
        src = install_root / self.ARTIFACT_PATHS[requested_tool]
        return ctx.stage_binary(requested_tool, src, self.source_label(ctx))


PROVIDER = SquashfuseProvider()
