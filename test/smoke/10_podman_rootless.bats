#!/usr/bin/env bats

load test_helper.bash
load /usr/local/lib/bats-support/load
load /usr/local/lib/bats-assert/load

setup_file() {
  smoke_require_cmds_or_skip podman parallax parallax-mount-program mksquashfs
  smoke_init_file_env
}

teardown_file() {
  smoke_cleanup_file_env
}

@test "rootless podman can run a container" {
  run podman --version
  assert_success
  assert_output --regexp '^podman version .+$'

  run podman info
  assert_success

  run podman run --rm docker.io/library/alpine:3.20 echo "ok (rootless podman)"
  assert_success
  assert_output --partial "ok (rootless podman)"
}

@test "parallax migrates busybox into a ro store" {
  run "$PODMAN_BINARY" --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" pull busybox:latest
  assert_success

  run "$PARALLAX_BINARY" \
    --podmanRoot "$PODMAN_ROOT" \
    --roStoragePath "$RO_STORAGE" \
    --mksquashfsPath "$MKSQUASHFS_PATH" \
    --log-level info \
    --migrate \
    --image busybox:latest
  assert_success
}

@test "podman sees busybox in the parallax ro store" {
  smoke_prepare_busybox_ro_store

  run "$PODMAN_BINARY" \
    --root "$CLEAN_ROOT" \
    --runroot "$PODMAN_RUNROOT" \
    --storage-opt additionalimagestore="$RO_STORAGE" \
    --storage-opt mount_program="$MOUNT_PROGRAM_PATH" \
    images
  assert_success
  assert_output --partial "busybox"
}

@test "podman runs busybox from the parallax ro store" {
  smoke_prepare_busybox_ro_store

  run "$PODMAN_BINARY" \
    --root "$CLEAN_ROOT" \
    --runroot "$PODMAN_RUNROOT" \
    --storage-opt additionalimagestore="$RO_STORAGE" \
    --storage-opt mount_program="$MOUNT_PROGRAM_PATH" \
    run --rm busybox:latest echo "ok (parallax ro-store)"
  assert_success
  assert_output "ok (parallax ro-store)"
}

@test "podman hpc module supports keep-id against the ro store" {
  smoke_prepare_busybox_ro_store

  run "$PODMAN_BINARY" --module hpc \
    --root "$CLEAN_ROOT" \
    --runroot "$PODMAN_RUNROOT" \
    --storage-opt additionalimagestore="$RO_STORAGE" \
    --storage-opt mount_program="$MOUNT_PROGRAM_PATH" \
    run --rm --userns=keep-id \
    --runtime-flag log=/tmp/crun.log \
    busybox:latest stat -c '%u %g %n' /bin/sh
  assert_success
  assert_output --regexp '^[0-9]+ [0-9]+ /bin/sh$'
}
