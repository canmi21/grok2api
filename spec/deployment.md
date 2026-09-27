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

## The image carries grok2api; the CLI lives on the volume and moves on its own

xAI ships the CLI weekly on its stable channel, and grok2api changes far less often. An image
holding both would have to be rebuilt and redeployed at xAI's pace -- a release train kept running
for somebody else's releases. So the image holds grok2api and a seed copy of the CLI, and the CLI
that runs is the one on the persistent volume, updated there while the container keeps running.
The image is released when grok2api changes, and at no other time.

A CLI baked into each image, rebuilt by a scheduled CI job, was the other way. Its immutability
buys little here: whichever way a new CLI arrives, it can change the agent's behavior, and CI has
no signed-in account to catch that with. What catches it is the startup check in
[bridge.md](bridge.md), which runs against the real account, and that works the same on a volume.

**grok2api does the updating, not the CLI.** The CLI can update itself, but grok2api turns that off
and runs the cycle itself, because a pinned version needs the same machinery anyway and one path
is easier to trust than two:

1. Read the target version: the stable channel's pointer, or the pinned version when the setting
   names one.
2. When it differs from the running version, download that version's binary for the container's
   platform onto the volume, beside the one running.
3. Start a second agent on it and run the startup check against it.
4. If the check passes, new sessions go to the new agent, and the old one is stopped once its
   sessions have expired. If it fails, the new binary is discarded, the old agent carries on, and
   the failure is logged with the version that caused it.

The binary that last passed the check is kept, so a bad release never leaves the volume without a
working CLI. The seed copy in the image is only for a volume that has none yet, so a first start
needs no network to reach a working agent.

Following the stable channel is the default. The pin is the escape hatch for a release that
fails the check, or that passes it and still misbehaves.

## The container signs in once, and Grok keeps it signed in

The CLI's state, credentials included, lives in the container's `GROK_HOME` on a persistent
volume. Signing in is done once, by the CLI, with its device-code flow (`grok login --device-auth`),
which needs no browser on the server. From then on the CLI refreshes its own credentials, which is
the division of labor [bridge.md](bridge.md) requires.

**`grok2api login` is how, and a signed-out server waits for it.** The subcommand runs the CLI's
device-code sign-in against exactly the environment the server uses -- its `HOME`, its
`GROK_HOME`, its binary -- so nobody has to reproduce those by hand inside a container. A server
that starts signed out does not exit: it says so and looks again every few seconds, so signing in
is `grok2api login` in the running container and nothing more. Exiting instead would put the
container in a restart loop that has to be caught between restarts to be signed in at all.

**A development machine runs its own CLI, and nothing updates it.** Naming a binary
(`GROK2API_GROK_BIN`) turns the managed cycle off: that binary belongs to whatever installed it,
Homebrew on the machine this is written on, and a second updater on it would fight the first.
