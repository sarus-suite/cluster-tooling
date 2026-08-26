.PHONY: dev

dev:
	@version=$$(awk -F '"' '$$1 ~ /^version[[:space:]]*=/ { print $$2; exit }' crates/sarusctl/Cargo.toml); \
	hash=$$(git rev-parse --short=12 HEAD); \
	if [ -n "$$(git status --porcelain --untracked-files=all)" ]; then hash="$$hash.dirty"; fi; \
	SARUSCTL_VERSION="$$version-dev+g$$hash" cargo build --locked -p sarusctl --release
