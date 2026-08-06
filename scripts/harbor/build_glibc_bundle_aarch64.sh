#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /absolute/path/to/gents-glibc-bullseye-aarch64.tar.gz" >&2
  exit 2
fi

output_path=$1
case "${output_path}" in
  /*) ;;
  *)
    echo "output path must be absolute" >&2
    exit 2
    ;;
esac

output_dir=$(dirname "${output_path}")
output_name=$(basename "${output_path}")
case "${output_name}" in
  *[!A-Za-z0-9._-]*)
    echo "output filename contains unsupported characters" >&2
    exit 2
    ;;
esac

mkdir -p "${output_dir}"
docker run --rm --platform linux/arm64 \
  -e GENTS_BUNDLE_OUTPUT_NAME="${output_name}" \
  -v "${output_dir}:/out" \
  debian:bullseye-slim \
  sh -c '
    set -eu
    apt-get update >/dev/null
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      liblzma5 libssl1.1 libstdc++6 >/dev/null
    mkdir -p /tmp/gents-glibc
    cp -L \
      /lib/ld-linux-aarch64.so.1 \
      /lib/aarch64-linux-gnu/libc.so.6 \
      /lib/aarch64-linux-gnu/libdl.so.2 \
      /lib/aarch64-linux-gnu/libgcc_s.so.1 \
      /lib/aarch64-linux-gnu/libm.so.6 \
      /lib/aarch64-linux-gnu/libnss_dns.so.2 \
      /lib/aarch64-linux-gnu/libnss_files.so.2 \
      /lib/aarch64-linux-gnu/libpthread.so.0 \
      /lib/aarch64-linux-gnu/libresolv.so.2 \
      /lib/aarch64-linux-gnu/libutil.so.1 \
      /lib/aarch64-linux-gnu/liblzma.so.5 \
      /usr/lib/aarch64-linux-gnu/libcrypto.so.1.1 \
      /usr/lib/aarch64-linux-gnu/libssl.so.1.1 \
      /usr/lib/aarch64-linux-gnu/libstdc++.so.6 \
      /tmp/gents-glibc/
    tar -C /tmp/gents-glibc -czf "/out/${GENTS_BUNDLE_OUTPUT_NAME}" .
  '

echo "wrote ${output_path}"
