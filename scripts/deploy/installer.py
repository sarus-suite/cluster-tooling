#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import sys
import tempfile
from pathlib import Path
from typing import List

try:
    from installer_modules.core import (
        DEFAULT_CACHE_DIR,
        DEFAULT_INSTALL_PREFIX,
        DEFAULT_STATE_ROOT,
        DEFAULT_SUPPORT_ROOT,
        HostToolsInstaller,
        InstallerConfig,
        InstallerError,
        load_manifest,
    )
except ModuleNotFoundError as exc:
    if exc.name == "yaml":
        print(
            "missing python dependency: PyYAML. Install it with "
            "'python3 -m pip install -r scripts/deploy/requirements.txt' "
            "or your distro package manager before using this installer.",
            file=sys.stderr,
        )
        raise SystemExit(2) from exc
    raise


def parse_args(argv: List[str]) -> InstallerConfig:
    def expand_path(path: Path) -> Path:
        return Path(os.path.expandvars(str(path))).expanduser()

    parser = argparse.ArgumentParser(
        description="Manifest-driven installer for common host tools and Sarus Suite tools."
    )
    parser.add_argument("mode", choices=("common", "sarus"))
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--cache-dir", type=Path, default=DEFAULT_CACHE_DIR)
    parser.add_argument("--install-prefix", type=Path, default=DEFAULT_INSTALL_PREFIX)
    parser.add_argument("--support-root", type=Path, default=DEFAULT_SUPPORT_ROOT)
    parser.add_argument("--state-root", type=Path, default=DEFAULT_STATE_ROOT)
    parser.add_argument("--stage-root", type=Path)
    parser.add_argument("--write-manifest", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    ns = parser.parse_args(argv)

    stage_root = ns.stage_root
    if stage_root is None:
        stage_root = Path(tempfile.mkdtemp(prefix=f"{ns.mode}-installer-"))

    return InstallerConfig(
        mode=ns.mode,
        manifest_path=expand_path(ns.manifest).resolve(),
        cache_dir=expand_path(ns.cache_dir).resolve(),
        install_prefix=expand_path(ns.install_prefix),
        support_root=expand_path(ns.support_root),
        state_root=expand_path(ns.state_root).resolve(),
        stage_root=expand_path(stage_root).resolve(),
        write_manifest=expand_path(ns.write_manifest).resolve() if ns.write_manifest else None,
        dry_run=ns.dry_run,
    )


def main(argv: List[str]) -> int:
    try:
        config = parse_args(argv)
        manifest = load_manifest(config.manifest_path)
        HostToolsInstaller(config, manifest).run()
        return 0
    except InstallerError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
