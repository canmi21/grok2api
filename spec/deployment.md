# Where it runs

## A container on a server, not the development machine

grok2api is built to be packaged as a Docker image and run on a server. The development machine is
where it is written and tested; nothing about it is meant to be installed there as a service.
That decides several things elsewhere: the server listens on every interface
([api.md](api.md)), and the CLI's credentials are the container's own rather than borrowed from
anyone's home directory.

## The CLI has a Linux build for both architectures

xAI publishes the CLI as a single binary per platform, resolved by
`https://x.ai/cli/install.sh`: the channel pointer `https://x.ai/cli/<channel>` names a version,
and `https://x.ai/cli/grok-<version>-<os>-<arch>` is the binary. Measured at 1.0.41:

- `linux-aarch64` and `linux-x86_64` exist; there is no musl variant, and none is needed, because
  the aarch64 binary is statically linked.
- It runs unmodified in `debian:stable-slim`, and reaches the network there with nothing added to
  the image.
- There is no APT repository. Installing is the script or the binary; updating is the script
  again or the CLI's own `grok update`.

## The container signs in once, and Grok keeps it signed in

The CLI's state, credentials included, lives in the container's `GROK_HOME` on a persistent
volume. Signing in is done once, by the CLI, with its device-code flow (`grok login --device-auth`),
which needs no browser on the server. From then on the CLI refreshes its own credentials, which is
the division of labor [bridge.md](bridge.md) requires.
