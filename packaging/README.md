# Packaging

A prebuilt Linux tarball, built the only way that is safe: inside the oldest
distribution it claims to support.

```bash
packaging/build.sh            # → dist/ragpilot-<version>-x86_64-linux-gnu.tar.gz
packaging/build.sh --verify   # also install it into a stock ubuntu:22.04 and run it
```

## Why a container

glibc is forward-compatible, not backward-compatible. A binary linked on a host
running glibc 2.44 records that it needs symbols versioned 2.38, 2.41, and so
on; Ubuntu 22.04 ships 2.35 and the loader refuses it before `main` is reached.
Compiling inside `ubuntu:22.04` means the newest symbol the binary can possibly
reference is one that 22.04 has.

Only the binary leaves the image. Nothing about the build host — paths,
toolchain version, environment — travels with it.

## What is in the tarball

| | |
|---|---|
| `ragpilot` | the binary, stripped |
| `install.sh` | the installer, POSIX `sh`, no dependencies |
| `README.md`, `LICENSE` | so the archive stands on its own |

`install.sh` chooses `/usr/local/bin` or `~/.local/bin` depending on what it is
allowed to write, refuses to install on the wrong architecture or an older
glibc, and runs `ragpilot --version` afterwards rather than assuming the copy
worked. `--uninstall` removes the binary and leaves your indexes and brain
alone.

## Verification

`--verify` is not a formality. Building in 22.04 proves the binary *links*
against 22.04; it does not prove it *runs* there, because the build image has a
compiler and its libraries and a user's machine has neither. The check unpacks
the tarball in a stock `ubuntu:22.04`, installs it, prints `ldd`, and makes the
binary answer. Run it before publishing a tarball.

## Runtime dependencies

Only what a stock Ubuntu 22.04 already has: `libssl3` and `ca-certificates`.
Qdrant is a separate service, not a library — the installer points you at it if
it is not up.
