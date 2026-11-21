#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 ]]; then
    echo "usage: $0 SOURCE_DIRECTORY EMPTY_OUTPUT_DIRECTORY" >&2
    exit 2
fi
source_dir=$(realpath "$1")
output=$(realpath -m "$2")
mkdir "$output"
export LC_ALL=C TZ=UTC SOURCE_DATE_EPOCH=1794787200
cd "$output"
gcc -static -no-pie -nostdlib -O2 -Wl,--build-id=none -o hello-raw "$source_dir/hello-raw.c"
gcc -static -no-pie -O2 -Wl,--build-id=none -o hello-glibc-static "$source_dir/hello.c"
musl-gcc -static -no-pie -O2 -Wl,--build-id=none -o hello-musl-static "$source_dir/hello.c"
HOME="$output" GOCACHE="$output/go-cache" CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
    go build -trimpath -buildvcs=false -ldflags=-buildid= -o hello-go "$source_dir/hello.go"
printf 'hello from linux\n' > expected.stdout
: > expected.stderr
printf '0\n' > expected.status
for program in hello-raw hello-glibc-static hello-musl-static hello-go; do
    readelf -h "$program" | grep -q 'Class:.*ELF64'
    readelf -h "$program" | grep -q 'Machine:.*Advanced Micro Devices X86-64'
    if readelf -l "$program" | grep -q INTERP; then
        echo "$program unexpectedly has an interpreter" >&2
        exit 1
    fi
    status=0
    "./$program" > "$program.stdout" 2> "$program.stderr" || status=$?
    printf '%s\n' "$status" > "$program.status"
    cmp expected.stdout "$program.stdout"
    cmp expected.stderr "$program.stderr"
    cmp expected.status "$program.status"
    readelf -h -l "$program" > "$program.elf.txt"
done
sha256sum hello-raw hello-glibc-static hello-musl-static hello-go expected.* > SHA256SUMS
(cd "$source_dir" && sha256sum hello-raw.c hello.c hello.go build-hellos.sh) > SOURCES.sha256
dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' gcc gcc-14 binutils libc6-dev musl musl-dev musl-tools golang-go golang-1.24-go > toolchain.tsv
cat SHA256SUMS
