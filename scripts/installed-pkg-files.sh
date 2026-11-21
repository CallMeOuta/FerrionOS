#!/usr/bin/env bash


export ARCH=$(uname -m)
export CONFIG_NAME=desktop

make unmount &>/dev/null || true

make mount >/dev/null

find "build/${ARCH}/${CONFIG_NAME}/" -type f | cut -d / -f5- |\
sort |\
uniq |\
while read path
do
    if [ -z "${path}" ]
    then
        continue
    fi

    pkgs="$(
        find recipes/*"/target/${ARCH}-unknown-redox/stage/${path}" 2>/dev/null |
        cut -d/ -f3 |
        tr '\n' ' ' |
        sort |
        uniq
    )"
    if [ -n "${pkgs}" ]
    then
        echo "$path: ${pkgs}"
    else
        echo "$path: no packages, see config/${ARCH}/${CONFIG_NAME}.toml"
    fi
done

make unmount &>/dev/null || true
