from __future__ import annotations

from ..core import InstallContext, InstallRecord, ToolProvider


class ParallaxProvider(ToolProvider):
    DEFAULT_VERSION = "0.10.2"
    DEFAULT_PLATFORM = "ubuntu-24.04"
    FILENAME_TEMPLATES = {
        "parallax": "parallax-v{version}-{platform}-{arch}",
        "parallax-mount-program": "parallax-mount-program-v{version}.sh",
    }

    def __init__(self) -> None:
        super().__init__(name="parallax", domain="sarus", tools=list(self.FILENAME_TEMPLATES))

    def install(self, requested_tool: str, ctx: InstallContext) -> InstallRecord:
        version = ctx.resolve_source("parallax_version", self.DEFAULT_VERSION)
        url = (
            "https://github.com/sarus-suite/parallax/releases/download/"
            f"v{version}/"
            f"{self.FILENAME_TEMPLATES[requested_tool].format(version=version, platform=self.DEFAULT_PLATFORM, arch=ctx.arch)}"
        )
        src = ctx.download(url, requested_tool)
        return ctx.stage_binary(requested_tool, src, f"parallax:{version}")


PROVIDER = ParallaxProvider()
