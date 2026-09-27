# grok2api and a seed copy of the Grok CLI. The CLI that runs is the one on the /data volume,
# which grok2api keeps up to date itself; the seed is only for a volume that has none yet.
# See spec/deployment.md.

# The compiler the workspace pins in its rust-toolchain.toml. The build context is this
# repository alone, so the pin is stated again here.
ARG RUST_VERSION=1.98.1

FROM rust:${RUST_VERSION}-slim-trixie AS build
WORKDIR /src
# Dependencies first, against an empty main, so a source change does not rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
	&& cargo build --release --locked \
	&& rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

# Whichever version the stable channel names when the image is built, for the platform it is
# built for. Run once here so a binary that does not start fails the build, not the first boot.
FROM debian:trixie-slim AS seed
ARG TARGETARCH
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates curl \
	&& rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
		arm64) platform=linux-aarch64 ;; \
		amd64) platform=linux-x86_64 ;; \
		*) echo "no Grok CLI is published for $TARGETARCH" >&2; exit 1 ;; \
	esac \
	&& version="$(curl -fsSL https://x.ai/cli/stable | head -n1 | tr -d '[:space:]')" \
	&& curl -fsSL "https://x.ai/cli/grok-${version}-${platform}" -o /grok \
	&& chmod 755 /grok \
	&& /grok --version

FROM debian:trixie-slim
# ca-certificates: grok2api's own HTTPS, to fetch newer CLIs, verifies against the system store.
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates \
	&& rm -rf /var/lib/apt/lists/* \
	&& useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin grok2api \
	&& mkdir /data \
	&& chown grok2api:grok2api /data
COPY --from=seed /grok /usr/local/lib/grok2api/grok
COPY --from=build /src/target/release/grok2api /usr/local/bin/grok2api
USER grok2api
ENV GROK2API_DATA_DIR=/data \
	GROK2API_PORT=8000
VOLUME /data
EXPOSE 8000
ENTRYPOINT ["grok2api"]
