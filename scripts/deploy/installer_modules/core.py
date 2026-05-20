from __future__ import annotations

import importlib
import json
import pkgutil
import platform
import shutil
import subprocess
import tarfile
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple

import yaml


REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_CACHE_DIR = REPO_ROOT / ".deploy-cache" / "host-tools-installer"
DEFAULT_STATE_ROOT = REPO_ROOT / ".deploy-out" / "install-manifests"
DEFAULT_INSTALL_PREFIX = Path.home() / ".local" / "bin"
DEFAULT_SUPPORT_ROOT = Path.home() / ".local" / "share" / "cluster-tooling" / "host-tools"


@dataclass
class InstallerConfig:
    mode: str
    manifest_path: Path
    cache_dir: Path
    install_prefix: Path
    support_root: Path
    state_root: Path
    stage_root: Path
    write_manifest: Optional[Path] = None
    dry_run: bool = False


# For tracking installed tools
@dataclass
class InstallRecord:
    tool: str
    source: str
    staged_files: List[str] = field(default_factory=list)
    installed_files: List[str] = field(default_factory=list)
    notes: List[str] = field(default_factory=list)


# for tracking non-tools (dirs and config files)
@dataclass
class SupportRecord:
    name: str
    source: str
    staged_files: List[str] = field(default_factory=list)
    installed_files: List[str] = field(default_factory=list)
    notes: List[str] = field(default_factory=list)


class InstallerError(RuntimeError):
    pass


@dataclass
class ToolProvider:
    name: str
    domain: str
    tools: List[str]

    def install(self, requested_tool: str, ctx: "InstallContext") -> InstallRecord:
        raise NotImplementedError


# backbone of the installer, and provides some common helper functions:
#   - resolve_source, download, unpack_tarball, run_command (for building), stage_binary, stage_file, stage_tree_once
class InstallContext:
    def __init__(self, config: InstallerConfig, manifest: dict):
        self.config = config
        self.manifest = manifest
        self.records: List[InstallRecord] = []
        self.support_records: List[SupportRecord] = []
        self.active_tools = self._resolve_active_tools()
        self.bundle_state: Dict[str, object] = {}
        self.arch = self._detect_architecture()

    def log(self, message: str) -> None:
        print(f"[{self.config.mode}-installer] {message}")

    def _detect_architecture(self) -> str:
        machine = platform.machine().lower()
        arch_aliases = {
            "x86_64": "amd64",
            "amd64": "amd64",
            "aarch64": "arm64",
            "arm64": "arm64",
        }
        if machine not in arch_aliases:
            raise InstallerError(
                f"unsupported host architecture '{machine}'; expected one of: "
                + ", ".join(sorted(arch_aliases))
            )
        return arch_aliases[machine]

    def resolve_source(self, key: str, default: Optional[str] = None) -> str:
        sources = self.manifest.get("sources", {})
        if key in sources:
            return str(sources[key])
        if default is not None:
            return default
        raise InstallerError(f"manifest is missing required source setting '{key}'")

    def ensure_dir(self, path: Path) -> None:
        if self.config.dry_run:
            return
        path.mkdir(parents=True, exist_ok=True)

    def download(self, url: str, filename: str) -> Path:
        downloads_dir = self.config.cache_dir / "downloads"
        dest = downloads_dir / filename
        self.log(f"downloading {url}")
        if not self.config.dry_run:
            downloads_dir.mkdir(parents=True, exist_ok=True)
            if not dest.exists():
                urllib.request.urlretrieve(url, dest)
        return dest

    def unpack_tarball(self, tarball: Path, destination: Path) -> None:
        if self.config.dry_run:
            return
        shutil.rmtree(destination, ignore_errors=True)
        destination.mkdir(parents=True, exist_ok=True)
        with tarfile.open(tarball, "r:gz") as archive:
            archive.extractall(destination)

    def run_command(self, argv: List[str], cwd: Path) -> None:
        self.log(f"running {' '.join(argv)} in {cwd}")
        if not self.config.dry_run:
            try:
                subprocess.run(argv, cwd=cwd, check=True)
            except FileNotFoundError as exc:
                command = argv[0] if argv else "<empty>"
                raise InstallerError(
                    f"required build command '{command}' was not found in PATH; "
                    "install the system build prerequisites before running the installer"
                ) from exc

    def stage_binary(
        self, tool: str, src: Path, source: str, note: Optional[str] = None
    ) -> InstallRecord:
        staged_path = self.config.stage_root / "bin" / tool
        installed_path = self.config.install_prefix / tool

        if not src.exists() and not self.config.dry_run:
            raise InstallerError(f"expected artifact for {tool} at {src} but it was not found")

        if not self.config.dry_run:
            staged_path.parent.mkdir(parents=True, exist_ok=True)
            installed_path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, staged_path)
            staged_path.chmod(0o755)
            shutil.copy2(staged_path, installed_path)
            installed_path.chmod(0o755)

        notes = [note] if note else []
        return InstallRecord(
            tool=tool,
            source=source,
            staged_files=[str(staged_path)],
            installed_files=[str(installed_path)],
            notes=notes,
        )

    def stage_script(
        self, tool: str, content: str, source: str, note: Optional[str] = None
    ) -> InstallRecord:
        staged_path = self.config.stage_root / "bin" / tool
        installed_path = self.config.install_prefix / tool

        if not self.config.dry_run:
            staged_path.parent.mkdir(parents=True, exist_ok=True)
            installed_path.parent.mkdir(parents=True, exist_ok=True)
            staged_path.write_text(content, encoding="utf-8")
            staged_path.chmod(0o755)
            shutil.copy2(staged_path, installed_path)
            installed_path.chmod(0o755)

        notes = [note] if note else []
        return InstallRecord(
            tool=tool,
            source=source,
            staged_files=[str(staged_path)],
            installed_files=[str(installed_path)],
            notes=notes,
        )

    def stage_file(
        self,
        name: str,
        content: str,
        relative_path: Path,
        source: str,
        note: Optional[str] = None,
    ) -> SupportRecord:
        stage_dst = self.config.stage_root / "support" / relative_path
        final_dst = self.config.support_root / relative_path

        if not self.config.dry_run:
            stage_dst.parent.mkdir(parents=True, exist_ok=True)
            final_dst.parent.mkdir(parents=True, exist_ok=True)
            stage_dst.write_text(content, encoding="utf-8")
            shutil.copy2(stage_dst, final_dst)

        notes = [note] if note else []
        return SupportRecord(
            name=name,
            source=source,
            staged_files=[str(stage_dst)],
            installed_files=[str(final_dst)],
            notes=notes,
        )

    def stage_tree_once(
        self,
        state_key: str,
        name: str,
        src: Path,
        relative_root: Path,
        source: str,
        note: Optional[str] = None,
    ) -> SupportRecord:
        cached = self.bundle_state.get(state_key)
        if isinstance(cached, SupportRecord):
            return cached

        if self.config.dry_run:
            record = SupportRecord(
                name=name,
                source=source,
                staged_files=[str(self.config.stage_root / "support" / relative_root)],
                installed_files=[str(self.config.support_root / relative_root)],
                notes=[note] if note else [],
            )
            self.bundle_state[state_key] = record
            return record

        stage_dst = self.config.stage_root / "support" / relative_root
        final_dst = self.config.support_root / relative_root
        self.copy_tree(src, stage_dst)
        self.copy_tree(stage_dst, final_dst)
        record = SupportRecord(
            name=name,
            source=source,
            staged_files=[str(stage_dst)],
            installed_files=[str(final_dst)],
            notes=[note] if note else [],
        )
        self.bundle_state[state_key] = record
        return record

    def remember_record(self, record: InstallRecord) -> None:
        self.records.append(record)

    def remember_support_record(self, record: SupportRecord) -> None:
        if any(
            existing.name == record.name and existing.installed_files == record.installed_files
            for existing in self.support_records
        ):
            return
        self.support_records.append(record)

    def copy_tree(self, src: Path, dst: Path) -> None:
        dst.mkdir(parents=True, exist_ok=True)
        for entry in src.iterdir():
            entry_dst = dst / entry.name
            if entry.is_dir():
                shutil.copytree(entry, entry_dst, dirs_exist_ok=True)
            else:
                shutil.copy2(entry, entry_dst)

    def find_single_child(self, root: Path) -> Path:
        children = [path for path in root.iterdir()]
        if len(children) != 1:
            raise InstallerError(
                f"expected one unpacked directory under {root}, found {len(children)}"
            )
        return children[0]

    def _resolve_active_tools(self) -> Set[str]:
        profiles = self.manifest.get("profiles", {})
        if not isinstance(profiles, dict):
            raise InstallerError("manifest field 'profiles' must be a mapping")

        active_tools: Set[str] = set()
        for profile_name, profile in profiles.items():
            if not isinstance(profile, dict):
                raise InstallerError(f"profile '{profile_name}' must be a mapping")
            if not profile.get("enabled", False):
                continue
            tools = profile.get("tools", [])
            if not isinstance(tools, list):
                raise InstallerError(f"profile '{profile_name}'.tools must be a list")
            for tool in tools:
                if not isinstance(tool, str):
                    raise InstallerError(
                        f"profile '{profile_name}' contains a non-string tool entry"
                    )
                active_tools.add(tool)
        return active_tools


# Here we auto discover providers, which dynamically loads providers and associate it to a unique tool
def discover_providers() -> Tuple[List[ToolProvider], Dict[str, ToolProvider]]:
    from . import providers as providers_pkg

    providers: List[ToolProvider] = []
    tool_map: Dict[str, ToolProvider] = {}
    discovered_modules = sorted(pkgutil.iter_modules(providers_pkg.__path__), key=lambda item: item.name)

    for module_info in discovered_modules:
        if module_info.name.startswith("_"):
            continue

        module = importlib.import_module(f"{providers_pkg.__name__}.{module_info.name}")
        provider = getattr(module, "PROVIDER", None)
        if provider is None:
            raise InstallerError(
                f"provider module '{module.__name__}' does not export a PROVIDER object"
            )
        if not isinstance(provider, ToolProvider):
            raise InstallerError(
                f"provider module '{module.__name__}' exported an invalid PROVIDER object"
            )
        if not provider.tools:
            raise InstallerError(f"provider '{provider.name}' does not declare any tools")

        providers.append(provider)
        for tool in provider.tools:
            existing = tool_map.get(tool)
            if existing is not None:
                raise InstallerError(
                    f"tool '{tool}' is owned by both provider '{existing.name}' "
                    f"and provider '{provider.name}'"
                )
            tool_map[tool] = provider

    if not providers:
        raise InstallerError("no installer providers were discovered")

    return providers, tool_map


class HostToolsInstaller:
    def __init__(self, config: InstallerConfig, manifest: dict):
        self.ctx = InstallContext(config, manifest)
        self.providers, self.tool_map = discover_providers()

    def run(self) -> None:
        self._validate_manifest()
        self._prepare_dirs()
        self.ctx.log(f"discovered providers: {', '.join(provider.name for provider in self.providers)}")
        self.ctx.log(f"active tools: {', '.join(sorted(self.ctx.active_tools)) or '(none)'}")

        for tool_name in sorted(self.ctx.active_tools):
            provider = self.tool_map[tool_name]
            record = provider.install(tool_name, self.ctx)
            self.ctx.remember_record(record)

        manifest_path = self._write_install_manifest()
        self.ctx.log(f"installation manifest written to {manifest_path}")

    def _validate_manifest(self) -> None:
        sources = self.ctx.manifest.get("sources", {})
        if not isinstance(sources, dict):
            raise InstallerError("manifest field 'sources' must be a mapping")

        unknown = sorted(tool for tool in self.ctx.active_tools if tool not in self.tool_map)
        if unknown:
            raise InstallerError(f"unknown tools in manifest: {', '.join(unknown)}")

        wrong_domain = sorted(
            tool
            for tool in self.ctx.active_tools
            if self.tool_map[tool].domain != self.ctx.config.mode
        )
        if wrong_domain:
            if self.ctx.config.mode == "common":
                raise InstallerError(
                    "common installer must not manage Sarus Suite components: "
                    + ", ".join(wrong_domain)
                )
            raise InstallerError(
                "sarus installer received non-Sarus tools: " + ", ".join(wrong_domain)
            )

    def _prepare_dirs(self) -> None:
        for path in [
            self.ctx.config.cache_dir,
            self.ctx.config.stage_root,
            self.ctx.config.install_prefix,
            self.ctx.config.support_root,
            self.ctx.config.state_root,
        ]:
            self.ctx.ensure_dir(path)

    def _write_install_manifest(self) -> Path:
        output_path = self.ctx.config.write_manifest or (
            self.ctx.config.state_root / f"{self.ctx.config.mode}-host-tools-install.json"
        )
        payload = {
            "mode": self.ctx.config.mode,
            "manifest": str(self.ctx.config.manifest_path),
            "install_prefix": str(self.ctx.config.install_prefix),
            "support_root": str(self.ctx.config.support_root),
            "stage_root": str(self.ctx.config.stage_root),
            "tools": [asdict(record) for record in self.ctx.records],
            "support_artifacts": [asdict(record) for record in self.ctx.support_records],
        }
        if not self.ctx.config.dry_run:
            output_path.parent.mkdir(parents=True, exist_ok=True)
            output_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        return output_path


def load_manifest(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as fh:
        data = yaml.safe_load(fh)
    if not isinstance(data, dict):
        raise InstallerError("manifest root must be a mapping")
    return data
