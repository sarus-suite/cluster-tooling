#!/usr/bin/python3
"""Controlled transfer tools for sarusctl tests; no network or real storage."""
import hashlib
import json
import os
import pathlib
import sys
import subprocess

directory = pathlib.Path(os.environ["PULL_FIXTURE"])
arguments = sys.argv[1:]
tool = pathlib.Path(sys.argv[0]).name


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode()


def descriptor(content, media_type):
    return {
        "mediaType": media_type,
        "digest": "sha256:" + hashlib.sha256(content).hexdigest(),
        "size": len(content),
    }


if arguments[:1] == ["--prepare"]:
    layout = directory / "producer"
    blobs = layout / "blobs" / "sha256"
    blobs.mkdir(parents=True, exist_ok=True)
    layer = b"synthetic layer bytes"
    config = encoded({
        "os": "linux", "architecture": "amd64", "revision": arguments[1],
        "rootfs": {"type": "layers", "diff_ids": []},
    })
    manifest_type = "application/vnd.oci.image.manifest.v1+json"
    manifest = encoded({
        "schemaVersion": 2, "mediaType": manifest_type,
        "config": descriptor(config, "application/vnd.oci.image.config.v1+json"),
        "layers": [descriptor(layer, "application/vnd.oci.image.layer.v1.tar+gzip")],
    })
    for content in [layer, config, manifest]:
        (blobs / hashlib.sha256(content).hexdigest()).write_bytes(content)
    root = descriptor(manifest, manifest_type)
    (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
    (layout / "index.json").write_bytes(encoded({"schemaVersion": 2, "manifests": [root]}))
    (directory / "manifest").write_bytes(manifest)
    (directory / "root").write_text(root["digest"])
    print(root["digest"])
    sys.exit(0)

with (directory / "calls").open("a") as log:
    log.write(json.dumps({
        "tool": tool, "args": arguments,
        "auth": os.getenv("REGISTRY_AUTH_FILE"),
        "storage": os.getenv("CONTAINERS_STORAGE_CONF"),
    }) + "\n")

failure = os.getenv("FAIL_STAGE")


def fail(stage):
    if failure == stage:
        sys.stderr.write(stage + " diagnostic\n")
        sys.exit(19)


reference = os.getenv("EXPECTED_REFERENCE", "registry.example/image@" + (directory / "root").read_text())
if tool == "hook":
    fail("hook")
elif tool == "skopeo":
    if "--help" in arguments:
        print("--raw --retry-times --preserve-digests --dest-shared-blob-dir --src-shared-blob-dir")
    elif arguments[0] == "inspect":
        fail("inspect")
        sys.stdout.buffer.write((directory / "manifest").read_bytes())
    elif arguments[0] == "copy":
        assert "--preserve-digests" in arguments
        assert "--all" not in arguments and "--format" not in arguments
        if arguments[-1].startswith("containers-storage:"):
            specifier, image = arguments[-1].removeprefix("containers-storage:[overlay@").split("]", 1)
            graphroot, runroot = specifier.split("+", 1)
            assert image == reference
            assert pathlib.Path(graphroot).is_absolute()
            assert pathlib.Path(graphroot).parent == pathlib.Path(runroot).parent
            pathlib.Path(graphroot).mkdir(parents=True, exist_ok=True)
            pathlib.Path(runroot).mkdir(parents=True, exist_ok=True)
            (directory / "export-storage").write_text(json.dumps({
                "graphroot": graphroot, "runroot": runroot,
            }))
            layer = pathlib.Path(graphroot) / "overlay" / "layer" / "diff"
            layer.mkdir(parents=True)
            (layer / "file").write_text("exported layer")
            layer.chmod(0o555)
            fail("export")
            assert "--src-shared-blob-dir" in arguments
            layout = pathlib.Path(arguments[-2][len("oci:"):])
            root = json.loads((layout / "index.json").read_text())["manifests"][0]
            assert root["digest"] == reference.split("@")[1]
            (directory / "exported").write_text(reference)
        else:
            fail("pull")
            assert arguments[-2] == "docker://" + reference
            layout = pathlib.Path(arguments[-1][len("oci:"):])
            producer = directory / "producer"
            for name in ["oci-layout", "index.json"]:
                (layout / name).write_bytes((producer / name).read_bytes())
            blobs = pathlib.Path(arguments[arguments.index("--dest-shared-blob-dir") + 1]) / "sha256"
            for blob in (producer / "blobs" / "sha256").iterdir():
                (blobs / blob.name).write_bytes(blob.read_bytes())
        print("captured transfer progress")
    else:
        raise AssertionError(arguments)
elif tool == "podman":
    assert "pull" not in arguments
    if "unshare" in arguments:
        graphroot = pathlib.Path(arguments[arguments.index("--root") + 1])
        runroot = pathlib.Path(arguments[arguments.index("--runroot") + 1])
        assert "--storage-driver" not in arguments
        instance = pathlib.Path(arguments[-3])
        assert graphroot == instance / "graphroot" == pathlib.Path(arguments[-2])
        assert runroot == instance / "runroot" == pathlib.Path(arguments[-1])
        assert instance.parent == pathlib.Path(os.environ["XDG_RUNTIME_DIR"]) / "sarus-suite"
        if os.getenv("FAIL_CLEANUP"):
            # Restore fixture permissions so TempDir can remove it after the test.
            subprocess.run(["chmod", "-R", "u+rwX", "--", str(instance)], check=True)
            sys.stderr.write("cleanup diagnostic\n")
            sys.exit(19)
        sys.exit(subprocess.run(arguments[arguments.index("unshare") + 1:]).returncode)
    elif "run" in arguments:
        fail("run")
        assert reference in arguments
        assert reference in json.loads((directory / "migrated-references").read_text())
        (directory / "executed").write_text(reference)
        sys.exit(int(os.getenv("WORKLOAD_EXIT_CODE", "0")))
    elif "kube" in arguments:
        manifest = pathlib.Path(arguments[-1])
        assert manifest.is_file()
        assert manifest.stat().st_mode & 0o777 == 0o600
        contents = manifest.read_bytes().decode()
        assert reference in contents
        assert "image: registry.example/image:release" not in contents
        phase = "play" if "play" in arguments else "down"
        (directory / ("manifest-" + phase)).write_text(contents)
        (directory / ("manifest-path-" + phase)).write_text(str(manifest))
        fail(phase)
    elif "exec" in arguments:
        assert "training-pod-app" in arguments
        sys.exit(int(os.getenv("WORKLOAD_EXIT_CODE", "0")))
    elif "container" in arguments:
        assert "exists" in arguments or "cleanup" in arguments
    elif "info" in arguments:
        fail("graphroot")
        print(directory / "graphroot")
    else:
        assert "image" in arguments and "exists" in arguments
        fail("podman")
        assert arguments[-1] == reference
        storage = json.loads((directory / "export-storage").read_text())
        assert arguments[arguments.index("--root") + 1] == storage["graphroot"]
        assert arguments[arguments.index("--runroot") + 1] == storage["runroot"]
        assert (directory / "exported").read_text() == reference
elif tool == "parallax":
    assert arguments[arguments.index("--image") + 1] == reference
    migrated = directory / "migrated-references"
    references = json.loads(migrated.read_text()) if migrated.exists() else []
    if "--migrate" in arguments:
        storage = json.loads((directory / "export-storage").read_text())
        assert arguments[arguments.index("--podmanRoot") + 1] == storage["graphroot"]
        assert (directory / "exported").read_text() == reference
        fail("migrate")
        (directory / "migrated").write_text(reference)
        migrated.write_text(json.dumps(sorted(set(references + [reference]))))
    else:
        assert "--exist" in arguments
        fail("exist")
        sys.exit(0 if reference in references else 1)
else:
    raise AssertionError(tool)
