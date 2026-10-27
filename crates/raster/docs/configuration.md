# Configuration selection

`load_config_xdg()` loads configuration from the first eligible directory in this
search order. `resolve_config_dir()` returns that directory without loading it.

1. `$XDG_CONFIG_HOME/sarus-suite`, or `$HOME/.config/sarus-suite` if
   `XDG_CONFIG_HOME` is unset and `HOME` is available.
2. Each absolute directory in the colon-separated `$XDG_CONFIG_DIRS`, with
   `/sarus-suite` appended, in the order listed.
3. `/etc/xdg/sarus-suite`, unless already included above.
4. `/etc/sarus-suite`, for compatibility with older installations.

A set but empty or relative `XDG_CONFIG_HOME` disables the user location. Empty or
relative entries in `XDG_CONFIG_DIRS` are ignored.

## Eligible directories and precedence

A directory is eligible when it contains at least one direct child that the
loader would attempt to read: a non-directory entry with a case-sensitive `.conf`
suffix and a UTF-8 name and path. Symlinks to files are supported; directories,
including symlinks to directories, are ignored. Subdirectories are not searched.
Empty and comment-only `.conf` files count; discovery does not parse their contents.

Missing directories and directories without applicable files are skipped. This
prevents an empty `$HOME/.config/sarus-suite` from hiding `/etc/sarus-suite` and
silently replacing system settings with defaults.

Only the selected directory is loaded. Its applicable files are processed in
alphabetical path order, with later values overriding earlier ones. Configuration
is **not merged across directories**: a partial user configuration still replaces
the system configuration, and unspecified settings use Raster's defaults.

Directory access or enumeration errors stop discovery, as does a candidate path
that exists but is not a directory. If no directory qualifies, discovery returns
an error. An eligible directory is selected before parsing; malformed, unreadable,
or unexpandable configuration files fail loading without falling back to another
directory. A broken `.conf` symlink is also attempted and fails loading.

## Explicit paths

`load_config()` and `load_config_path(None, ...)` retain the legacy behavior of
loading `/etc/sarus-suite` directly. `load_config_path(Some(path), ...)` loads the
specified directory directly. These APIs do not perform XDG discovery or require
an eligible file: an empty directory still yields default settings.

## Troubleshooting

Raster emits TRACE events for each examined directory, discarded candidates and
their reasons, the selected directory and an applicable file, and discovery
failures. Selection means eligible for loading, not successfully parsed. Events
contain paths and reasons, without configuration contents or environment dumps.

In `sarusctl`, enable these messages with:

```sh
sarusctl -vvv run ubuntu true
```

Check which directory is selected, especially when user settings appear to hide
system settings. Other consumers must enable TRACE events through their tracing
subscriber.
