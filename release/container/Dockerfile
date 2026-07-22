# syntax=docker/dockerfile:1.7

FROM ubuntu:24.04

ARG TARGETARCH
ARG GENTS_VERSION

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates \
        libgcc-s1 \
        liblzma5 \
        libssl3t64 \
        libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 gents \
    && useradd \
        --uid 10001 \
        --gid 10001 \
        --create-home \
        --home-dir /home/gents \
        --shell /usr/sbin/nologin \
        gents \
    && install -d -o gents -g gents /home/gents/.gents /workspace

COPY --chmod=0755 release/container/bin/gents-${TARGETARCH} /usr/local/bin/gents
COPY release/container/LICENSE /usr/share/licenses/gents/LICENSE

RUN version_output="$(/usr/local/bin/gents version)" \
    && printf '%s\n' "${version_output}" \
    && case "${version_output}" in \
        "gents ${GENTS_VERSION} ("*) ;; \
        *) echo "unexpected gents version: ${version_output}" >&2; exit 1 ;; \
    esac

USER gents
WORKDIR /workspace

VOLUME ["/home/gents/.gents"]
EXPOSE 9191 9292
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/gents"]
