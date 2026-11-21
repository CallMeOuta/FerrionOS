#!/usr/bin/env python3
# Build and boot an offline, pinned Linux GPU reference for FerrionOS.
# Downloads are data on the host; all downloaded programs run inside QEMU.

import argparse
import concurrent.futures
import contextlib
import gzip
import hashlib
import io
import json
import lzma
import os
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "scripts/gpu-reference"
KERNEL = "7.3.5-300.fc45.x86_64"
MESA = "26.2.7-1.fc45"
CURRENT_MESA = "26.3.1-1.fc46"
FEDORA_KEYS = {
    "45": "4F50A6114CD5C6976A7F1179655A4B02F577861E",
    "46": "D924B10D3E810DABDD8B56B596E7E91491211FCE",
}
# Fingerprints published at https://fedoraproject.org/security/.
# Each pin was authenticated through its RPM RSA header signature and the
# signed header's SHA-256 of the compressed payload. No scriptlets execute.
# The archive paths name exact builds, not a changing Fedora mirror.
KOJI = "https://kojipkgs.fedoraproject.org/packages/"
PACKAGE_PINS = """
9a3060e6686dd00a239a86e95140816968fa68a782cb385613dea620676c1dfb binutils/2.47/1.fc45/data/signed/f577861e/x86_64/binutils-2.47-1.fc45.x86_64.rpm
5b296721618dda3f2f794705d79b30fac2c5acd4998d82fb6a5137d4022fef29 busybox/1.37.0/9.fc45/data/signed/f577861e/x86_64/busybox-1.37.0-9.fc45.x86_64.rpm
0c4338556b818394570eef963fd183ab2bde7babe277de146f355c7595d36d8a bzip2/1.0.8/24.fc45/data/signed/f577861e/x86_64/bzip2-libs-1.0.8-24.fc45.x86_64.rpm
66fe200396e674df921ed78d496a6179d3ff2f586dcfb2b013cf4749155cd84b gcc/16.2.1/3.fc45/data/signed/f577861e/x86_64/cpp-16.2.1-3.fc45.x86_64.rpm
ca81198634a5f7909e7347feafb08fed68e549f311a2a936a00a6160f0694152 cyrus-sasl/2.1.28/37.fc45/data/signed/f577861e/x86_64/cyrus-sasl-lib-2.1.28-37.fc45.x86_64.rpm
86ca56dc35a1ca90a60ae458f627dbb3f440425ac8d80fa185bb6ebb8ce249df elfutils/0.197/1.fc45/data/signed/f577861e/x86_64/elfutils-debuginfod-client-0.197-1.fc45.x86_64.rpm
c74c8a9b5e0b9b4f7a07e0cada0be8fdddabf3718d50f2496f90a00f0f8748bd elfutils/0.197/1.fc45/data/signed/f577861e/x86_64/elfutils-libelf-0.197-1.fc45.x86_64.rpm
4a1196bc36a8ddae162d17d8158c52c137a027e7811d643b31da64ab04354bda elfutils/0.197/1.fc45/data/signed/f577861e/x86_64/elfutils-libs-0.197-1.fc45.x86_64.rpm
8e94a14880241d9eff840418124dcee0f862bd18356c85a5212fea4abed93773 expat/2.9.0/1.fc45/data/signed/f577861e/x86_64/expat-2.9.0-1.fc45.x86_64.rpm
333f2654a9c0ea00187df206f52640c9e8d2ae0b3b678438ec5f1dfc9bfdcb0a gcc/16.2.1/3.fc45/data/signed/f577861e/x86_64/gcc-16.2.1-3.fc45.x86_64.rpm
59e0f495ad41dee439f4cbaf192d00a195aead6cfc4ff2524730fec050f7c46e gdbm/1.23/12.fc45/data/signed/f577861e/x86_64/gdbm-libs-1.23-12.fc45.x86_64.rpm
8d6ec366a92f423acdc112c722b6ee9f81723fbcc6d60615401a09ec7fd0b007 glibc/2.44/2.fc45/data/signed/f577861e/x86_64/glibc-2.44-2.fc45.x86_64.rpm
6be4a896d3b9ff68884de273758d0ee66a0c5bf46677fbac2d7f5945bc8db203 glibc/2.44/2.fc45/data/signed/f577861e/x86_64/glibc-devel-2.44-2.fc45.x86_64.rpm
434884021f2e95f464950b6af962c79e7b3e9f3a89a17bc6a3e6307a555746f7 gmp/6.3.0/6.fc45/data/signed/f577861e/x86_64/gmp-6.3.0-6.fc45.x86_64.rpm
d393130f4ee3fbad5cf67182f1df314db098806613efbac52af17d73ed0414ae jansson/2.14/5.fc45/data/signed/f577861e/x86_64/jansson-2.14-5.fc45.x86_64.rpm
c55efa43359d11e4d987819a6a2c52032b35c1a4d56b97c9e251f211ed5205ad json-c/0.18/9.fc45/data/signed/f577861e/x86_64/json-c-0.18-9.fc45.x86_64.rpm
1bfb9cb805c20db34f1456a18a6cec9784afd5ea9b2343134600dcd2bfd479b1 kernel/7.3.5/300.fc45/data/signed/f577861e/x86_64/kernel-core-7.3.5-300.fc45.x86_64.rpm
d9c0f2177d23394dbb3733b55f175da61c2ef4c77bdbb737fd16fa0d7bd07646 kernel-headers/7.3.3/300.fc45/data/signed/f577861e/x86_64/kernel-headers-7.3.3-300.fc45.x86_64.rpm
3c5491fbf4fd5888b14da7f44063591210a0f3b89b0e3e5eedc9914777d12404 kernel/7.3.5/300.fc45/data/signed/f577861e/x86_64/kernel-modules-7.3.5-300.fc45.x86_64.rpm
a9836a038d848b388ad97ab1f6a5b144def9dedc609747e012524dadcbe0a7a5 kernel/7.3.5/300.fc45/data/signed/f577861e/x86_64/kernel-modules-core-7.3.5-300.fc45.x86_64.rpm
37c056b41062fa9b62355a03ac63cf923776625410f98b81a62ecef7f68b8037 keyutils/1.6.3/8.fc45/data/signed/f577861e/x86_64/keyutils-libs-1.6.3-8.fc45.x86_64.rpm
f3516bdce9ab3a99c9d8eafe9ca347ccbf922283e2d975549c27f96965139fde krb5/1.22.2/11.fc45/data/signed/f577861e/x86_64/krb5-libs-1.22.2-11.fc45.x86_64.rpm
b810820916a0008209a061b22e7369f7ef1d12ac7fa1c729399b5683b0194076 libX11/1.8.13/3.fc45/data/signed/f577861e/x86_64/libX11-1.8.13-3.fc45.x86_64.rpm
1f2b1b7f8fa4103c60887965d642ffe40fd88d2cdccd7da0ce4ef237e916a5b8 libX11/1.8.13/3.fc45/data/signed/f577861e/x86_64/libX11-xcb-1.8.13-3.fc45.x86_64.rpm
288187ea953c9f0f7443b3f310dea7af7643d6b7042055be519e459280c6a0ca libXau/1.0.12/5.fc45/data/signed/f577861e/x86_64/libXau-1.0.12-5.fc45.x86_64.rpm
598de1986395e8b81dcd1f8606c4698e4a306152e606959ed62beaa8b9fca4a3 libXext/1.3.6/6.fc45/data/signed/f577861e/x86_64/libXext-1.3.6-6.fc45.x86_64.rpm
1ceebf857628c48f053519f60b9073788729ef24626a2d2ee7f0fb6bb781f9dc libXxf86vm/1.1.6/5.fc45/data/signed/f577861e/x86_64/libXxf86vm-1.1.6-5.fc45.x86_64.rpm
64d2ee40ee5adebde915d36e9d4a17f1ddb93a638e52b38ff060235191fd3155 gcc/16.2.1/3.fc45/data/signed/f577861e/x86_64/libatomic-16.2.1-3.fc45.x86_64.rpm
ab95c31b5c0d88605b0d03fe78663d8521524d078f12fba1d9ef260a4febdf75 brotli/1.2.1/1.fc45/data/signed/f577861e/x86_64/libbrotli-1.2.1-1.fc45.x86_64.rpm
4aa0a66958124359269063f7268a1b5741ee1c6332b2078f5930a6b5a1686e8c libcbor/0.14.0/3.fc45/data/signed/f577861e/x86_64/libcbor-0.14.0-3.fc45.x86_64.rpm
95fde4bcc30c61dd5474687e2d27bb3deb4ad072dbc31581ef9f2b95df023532 e2fsprogs/1.47.4/2.fc45/data/signed/f577861e/x86_64/libcom_err-1.47.4-2.fc45.x86_64.rpm
43dca97dff92a063c1b9f3975bba37852369f84d42f1c171887a9ef016cf0b5b curl/8.21.0/9.fc45/data/signed/f577861e/x86_64/libcurl-8.21.0-9.fc45.x86_64.rpm
3c40d7f5be5c6679d31fdb9cd453e22b941a83bdb030311606e09e7a1ed1859d libdisplay-info/0.3.0/2.fc45/data/signed/f577861e/x86_64/libdisplay-info-0.3.0-2.fc45.x86_64.rpm
b6b6575c734d899dd25c2c6af93ce14195f2143467973f4544865371b6fc74f9 libdrm/2.4.134/2.fc45/data/signed/f577861e/x86_64/libdrm-2.4.134-2.fc45.x86_64.rpm
dbf5dc81b086a4fb6c96c73535620f19f6da4a51b351746e17e14bb1f6038e36 libdrm/2.4.134/2.fc45/data/signed/f577861e/x86_64/libdrm-devel-2.4.134-2.fc45.x86_64.rpm
90141313c7c0d632574ac138786c2a1fff7d9bbe70e881ff3ea858b58511fcbe libedit/3.1/60.20260512cvs.fc45/data/signed/f577861e/x86_64/libedit-3.1-60.20260512cvs.fc45.x86_64.rpm
c39698953327e53a5de2f703d4c6d8063e3580cc74dee3e14125319bfc204c6b libevent/2.1.13/1.fc45/data/signed/f577861e/x86_64/libevent-2.1.13-1.fc45.x86_64.rpm
e8c4cf4b1747f677b98e9f195c14c6abc8d70f1154d0d03f4d5cdeb110a77698 libffi/3.5.2/3.fc45/data/signed/f577861e/x86_64/libffi-3.5.2-3.fc45.x86_64.rpm
89eeea9bbd12ae8f45f37ccfc09fbc55fc7aff3a7fcea28de89e1933e0510762 libfido2/1.17.0/6.fc45/data/signed/f577861e/x86_64/libfido2-1.17.0-6.fc45.x86_64.rpm
57fc410c4b5939d5f85d42f90c5f0ab3514dbb74c965de1de7e3ac0d1374d606 gcc/16.2.1/3.fc45/data/signed/f577861e/x86_64/libgcc-16.2.1-3.fc45.x86_64.rpm
8d055193c8a2da0d04a7ae2d858333bfe46fd573f7483cfab732c75ed1cac256 libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-1.7.0-10.fc45.x86_64.rpm
b07312160cc0c894ddd870ecadf1aba88a37338fe3bdcae81d37146377ed1869 libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-core-devel-1.7.0-10.fc45.x86_64.rpm
622fca5b44d3296ae0e42a146ef71db878ae033fe68f995057fc291746bc28f6 libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-devel-1.7.0-10.fc45.x86_64.rpm
b44aed3592d9b767f02b02912b35454236527c12b14df2c359601aeb2c4f2e79 libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-egl-1.7.0-10.fc45.x86_64.rpm
4b1d802457bcee155a812ab64ebdf254c23f8e1eb1074428dd07e531ddca972e libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-gles-1.7.0-10.fc45.x86_64.rpm
352aa61d8b1f88cc4a61f2cfec06e4454c3bd7129b112785eeef6da019cf30c9 libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-glx-1.7.0-10.fc45.x86_64.rpm
99fbd1f1a250ef137687e3c20f8bf49c2ac772a36cefea17bfbf63bb2657a9ce libglvnd/1.7.0/10.fc45/data/signed/f577861e/x86_64/libglvnd-opengl-1.7.0-10.fc45.x86_64.rpm
33fb75d8b41fe17386154e07ce4e1d181ef16ea59edef254e90a69ecfaf911e1 gcc/16.2.1/3.fc45/data/signed/f577861e/x86_64/libgomp-16.2.1-3.fc45.x86_64.rpm
292b62009f71cc0bebfd8f1087397d507a4932ae3d081d8a1fb1b79f34705569 libidn2/2.3.8/4.fc45/data/signed/f577861e/x86_64/libidn2-2.3.8-4.fc45.x86_64.rpm
471e69c1cd9be651804a376ecee3cc3e1140da0c71ace50d4b5ab436a1f2923f libmpc/1.4.1/2.fc45/data/signed/f577861e/x86_64/libmpc-1.4.1-2.fc45.x86_64.rpm
4ad405fa966df0d1ba6602d4a7d205b9bbf0e54d369a9b96ce26d45c0a91e168 nghttp2/1.69.0/6.fc45/data/signed/f577861e/x86_64/libnghttp2-1.69.0-6.fc45.x86_64.rpm
310e0375cab96d116afdc61f8b425233c799dbedd36dc903f5fc3e188095f3ff nghttp3/1.19.0/1.fc45/data/signed/f577861e/x86_64/libnghttp3-1.19.0-1.fc45.x86_64.rpm
1992e2f0336e4c3b9b7d7f442b58ea527fc9d65d879b18790d58206fe5e2c49b libpciaccess/0.16/18.fc45/data/signed/f577861e/x86_64/libpciaccess-0.16-18.fc45.x86_64.rpm
82762074632ee1e59a01ebd14ca1ab9afbebeae8167b50350eebcccebbaffabc libpsl/0.23.1/1.fc45/data/signed/f577861e/x86_64/libpsl-0.23.1-1.fc45.x86_64.rpm
6420804a2eea7da60854ae14098c1b5c76bb44cda550abe2cf7962208097f23b libselinux/3.11/5.fc45/data/signed/f577861e/x86_64/libselinux-3.11-5.fc45.x86_64.rpm
cefb9aefcfb623b82e9d52f81d077f8b87bbed2897bcead7a7829355a1b66e7c libssh/0.12.2/1.fc45/data/signed/f577861e/x86_64/libssh-0.12.2-1.fc45.x86_64.rpm
c4256a01fab863cecadd3aaf24b015902ac4f3b841924550d788017e28aa2d9a gcc/16.2.1/3.fc45/data/signed/f577861e/x86_64/libstdc++-16.2.1-3.fc45.x86_64.rpm
606fc1533a11b9afa0a6cbcf08dac877921047eb3802d7cef58da2371ef770ef libtool/2.5.4/11.fc45/data/signed/f577861e/x86_64/libtool-ltdl-2.5.4-11.fc45.x86_64.rpm
6b59f73f3bb61694af9009c8925324b82dba7f957937eeadf6ed4ea58fc376a9 libunistring/1.1/12.fc45/data/signed/f577861e/x86_64/libunistring-1.1-12.fc45.x86_64.rpm
402222d9876b2dbebeffc85c995ce06669c70d1a12082cd67003e17ace1ddf5a util-linux/2.42.4/1.fc45/data/signed/f577861e/x86_64/libuuid-2.42.4-1.fc45.x86_64.rpm
ca11dc4d365f61ec2cc5a463b629311c0f58f97903497c9f760e0b410f96fbc8 libverto/0.3.2/13.fc45/data/signed/f577861e/x86_64/libverto-0.3.2-13.fc45.x86_64.rpm
aafd3738215ce5bbc66b1f9866cb790a494044e5aeb6c165b5d7e0a4e080e89e wayland/1.26.0/3.fc45/data/signed/f577861e/x86_64/libwayland-client-1.26.0-3.fc45.x86_64.rpm
200d8970f468df1c3867e4273343d83958c0e8592f663e777bdc572e9257f725 libxcb/1.17.0/8.fc45/data/signed/f577861e/x86_64/libxcb-1.17.0-8.fc45.x86_64.rpm
e1734d4aa06b440582bcca6f6f6769f0ac303d3ac59eee4bcc7ec3414c60de98 libxcrypt/4.5.2/4.fc45/data/signed/f577861e/x86_64/libxcrypt-4.5.2-4.fc45.x86_64.rpm
7c21dbf8e1dde792c35687b9a340b398b4640b1e2633a87cf6026975fd659456 libxml2/2.15.4/1.fc45/data/signed/f577861e/x86_64/libxml2-2.15.4-1.fc45.x86_64.rpm
31c2eb73ba93fe602eb9a90a8ff8f1278419c7f26d7d98b7d38a512f8b93f0a6 libxshmfence/1.3.2/9.fc45/data/signed/f577861e/x86_64/libxshmfence-1.3.2-9.fc45.x86_64.rpm
d637ecf99fffca9746ffe29a8b5f9db56a16a86dff6d637365e072f460f364a7 zstd/1.5.7/6.fc45/data/signed/f577861e/x86_64/libzstd-1.5.7-6.fc45.x86_64.rpm
352aacac2343474811146df7f7090c4334ec08ca72186987b46decd4aa7ccc81 llvm/23.1.5/2.fc45/data/signed/f577861e/x86_64/llvm-libs-23.1.5-2.fc45.x86_64.rpm
a4f6ce4dbb0285a234d3830a6f6d7e442b1b701f410c096bc8045d902314d21b lm_sensors/3.6.0/25.fc45/data/signed/f577861e/x86_64/lm_sensors-libs-3.6.0-25.fc45.x86_64.rpm
65e2787b78689ec2b2d88542d2355106f50795afdacb3bc975749889acf6238f mesa/26.2.7/1.fc45/data/signed/f577861e/x86_64/mesa-dri-drivers-26.2.7-1.fc45.x86_64.rpm
b8426c978be557473a3b1da150381b7f4d981a8f321863a16140dc5375a35a5e mesa/26.2.7/1.fc45/data/signed/f577861e/x86_64/mesa-filesystem-26.2.7-1.fc45.x86_64.rpm
a03a69a954fa86e7caeac20530c94a76eb743f3380d4171e7b6c31a5bc55ee2a mesa/26.2.7/1.fc45/data/signed/f577861e/x86_64/mesa-libEGL-26.2.7-1.fc45.x86_64.rpm
ca2ae8fdf36db645f08d2ba096e0f730ebdff53b0d6b113128f153ebc089c9f5 mesa/26.2.7/1.fc45/data/signed/f577861e/x86_64/mesa-libGL-26.2.7-1.fc45.x86_64.rpm
4df3cd9776644e2958a18bb725bd0e042e5825c9676be236a3ec9604c12231d0 mesa/26.2.7/1.fc45/data/signed/f577861e/x86_64/mesa-libgbm-26.2.7-1.fc45.x86_64.rpm
e644e5d7a4b6a80d38756e27add2b0416232fc794f9b25fafa94749eaac1442a mesa/26.2.7/1.fc45/data/signed/f577861e/x86_64/mesa-vulkan-drivers-26.2.7-1.fc45.x86_64.rpm
27d5661e3734dde39a290698b56b510d8d03bf632585864f1c2f47c7a7f52d10 mpdecimal/4.0.1/4.fc45/data/signed/f577861e/x86_64/mpdecimal-4.0.1-4.fc45.x86_64.rpm
5ce38596ccbbc9e761d613916f0c8f767db431274e84e05786d18d74f7f28975 mpfr/4.2.2/4.fc45/data/signed/f577861e/x86_64/mpfr-4.2.2-4.fc45.x86_64.rpm
bc8d79cdd67d96248c93bd1a3eec1e499ec2d1f96cb6922a709df6df06d5cd38 ncurses/6.6/3.fc45/data/signed/f577861e/x86_64/ncurses-libs-6.6-3.fc45.x86_64.rpm
a21c80bb88fbe95320028e486019b3ad998b0ded2320b3ccc458773b4b07a93c ngtcp2/1.22.1/3.fc45/data/signed/f577861e/x86_64/ngtcp2-1.22.1-3.fc45.x86_64.rpm
9306e5a00e6b6ec0f1ca0ccdc5914f4c89eb5625e68bb748fc2735051961f145 ngtcp2/1.22.1/3.fc45/data/signed/f577861e/x86_64/ngtcp2-crypto-ossl-1.22.1-3.fc45.x86_64.rpm
e7b345286682ed6836c10164b9a7f629823f2860c25cf810d07b33ce75c1694a openldap/2.6.16/1.fc45/data/signed/f577861e/x86_64/openldap-2.6.16-1.fc45.x86_64.rpm
4f8ece27fdbdbf94ced234b088e1f9eb6f1e60cf6732a898c2d8ad156dbc86ae openssl/4.0.3/1.fc45/data/signed/f577861e/x86_64/openssl-libs-4.0.3-1.fc45.x86_64.rpm
178b22ce6b2215257854742b8de5c37d78f50aa4db364d7088a3fc6dd3fbdc7d pcre2/10.49/1.fc45/data/signed/f577861e/x86_64/pcre2-10.49-1.fc45.x86_64.rpm
95c4a51c0e68cebadc5905ecf684eafb1f2c357615e5a37bc903cec414458709 python3.15/3.15.2/1.fc45/data/signed/f577861e/x86_64/python3-3.15.2-1.fc45.x86_64.rpm
cf21fa8f6d54c0d596fb973b0e810b7147de4e6864a721f96e74c0e5fa3dc6d2 python3.15/3.15.2/1.fc45/data/signed/f577861e/x86_64/python3-libs-3.15.2-1.fc45.x86_64.rpm
f47c0a94e9f775fa5711bada265880af7dc7d3de6404b817c94bc4bf9a8522ea readline/8.3/6.fc45/data/signed/f577861e/x86_64/readline-8.3-6.fc45.x86_64.rpm
ad5a47c05d8c45968188b48df729de143ab689b07bc65500fd3917db609eb578 spirv-tools/2026.2/3.fc45/data/signed/f577861e/x86_64/spirv-tools-libs-2026.2-3.fc45.x86_64.rpm
5bc6fb8d9a70a52c4392c95e0f4f47ad900c03260e6437230ccb8d56605be17b sqlite/3.54.1/2.fc45/data/signed/f577861e/x86_64/sqlite-libs-3.54.1-2.fc45.x86_64.rpm
c1af6d9a736f0337e14e219cff0f8fb9ed8a282bcf658c62efe6e8061f8692ab systemd/262.1/1.fc45/data/signed/f577861e/x86_64/systemd-libs-262.1-1.fc45.x86_64.rpm
ce6fd06b9a5aa01648824d814c1d9bfe42a613ce447dbd2a2deecfecbe1e5d90 vulkan-headers/1.4.350.0/4.fc45/data/signed/f577861e/noarch/vulkan-headers-1.4.350.0-4.fc45.noarch.rpm
1ae19395817707c550cb2652cf98b3fdf84511ac175619e4f376035ee6518925 vulkan-loader/1.4.350.0/2.fc45/data/signed/f577861e/x86_64/vulkan-loader-1.4.350.0-2.fc45.x86_64.rpm
e32f87eee997ffb44b2045b56ac7c5b9c0393e239edda9171e5dd350cb5413fa vulkan-loader/1.4.350.0/2.fc45/data/signed/f577861e/x86_64/vulkan-loader-devel-1.4.350.0-2.fc45.x86_64.rpm
587c17c60ddb7fb1553064c2533cf80a7ff1d7b2ad215cefe6e7d587a21109a5 xz/5.8.3/2.fc45/data/signed/f577861e/x86_64/xz-libs-5.8.3-2.fc45.x86_64.rpm
1bab05fecdd71fc4a946852d83405f238c89cccb63987c0941c7f2f0184a99e2 zlib-ng/2.3.3/6.fc45/data/signed/f577861e/x86_64/zlib-ng-compat-2.3.3-6.fc45.x86_64.rpm
"""
# Fedora 45 plus rawhide Mesa and its required glibc runtime upgrade.
CURRENT_PINS = """
f5da12a56dfb8b97a853877cca9d7c0f2e4f5ae66ab1054a0e77f55e52a08b53 glibc/2.44.9000/7.fc46/data/signed/91211fce/x86_64/glibc-2.44.9000-7.fc46.x86_64.rpm
0ada7c1087c3904750e90f84a060f07e2637cd83436026e4a29186675b2defcc mesa/26.3.1/1.fc46/data/signed/91211fce/x86_64/mesa-dri-drivers-26.3.1-1.fc46.x86_64.rpm
0ed1cd9d94cdd548e148f8cf3d53df4fc8f5a40a0f5c12e6dcbd2d98ba288fea mesa/26.3.1/1.fc46/data/signed/91211fce/x86_64/mesa-filesystem-26.3.1-1.fc46.x86_64.rpm
547e4ff68be1596fb806b76af5875f5eacfc9a8a629d575fcfc4d3297c13ad1a mesa/26.3.1/1.fc46/data/signed/91211fce/x86_64/mesa-libEGL-26.3.1-1.fc46.x86_64.rpm
078b31e508a0eed0d67abde2fd547a3a0ddb056d216e9e6628134e131224c5bf mesa/26.3.1/1.fc46/data/signed/91211fce/x86_64/mesa-libGL-26.3.1-1.fc46.x86_64.rpm
ca708259d6471ad9ccbe223714021391a58d297f42bf845fbf9cd15d09fe3fbe mesa/26.3.1/1.fc46/data/signed/91211fce/x86_64/mesa-libgbm-26.3.1-1.fc46.x86_64.rpm
fbb28a8b65280ad4ffcddc864e109be76dfd0544e9d014c2bf6fc4b782cef7b4 mesa/26.3.1/1.fc46/data/signed/91211fce/x86_64/mesa-vulkan-drivers-26.3.1-1.fc46.x86_64.rpm
"""
NATIVE_EVIDENCE = {'26.2.7': {'build_log_url': 'https://kojipkgs.fedoraproject.org/packages/mesa/26.2.7/1.fc45/data/logs/x86_64/build.log',
            'build_log_sha256': '94e95117548b4df4ca738992068eecdd41c332e2b200e517cf6c7376e0f1dd36',
            'meson_setup_line': 190,
            'meson_setup_command': '+ /usr/bin/meson setup --buildtype=plain --prefix=/usr '
                                   '--libdir=/usr/lib64 --libexecdir=/usr/libexec --bindir=/usr/bin '
                                   '--sbindir=/usr/bin --includedir=/usr/include --datadir=/usr/share '
                                   '--mandir=/usr/share/man --infodir=/usr/share/info '
                                   '--localedir=/usr/share/locale --sysconfdir=/etc --localstatedir=/var '
                                   '--sharedstatedir=/var/lib --wrap-mode=nodownload --auto-features=enabled '
                                   '. redhat-linux-build -Dplatforms=x11,wayland '
                                   '-Dgallium-drivers=llvmpipe,virgl,nouveau,r300,crocus,i915,iris,svga,radeonsi,r600,asahi,freedreno,etnaviv,tegra,vc4,v3d,lima,panfrost,zink,d3d12,ethosu,rocket '
                                   '-Dgallium-va=enabled -Dgallium-mediafoundation=disabled -Dteflon=true '
                                   '-Dgallium-rusticl=true '
                                   '-Dvulkan-drivers=swrast,amd,intel,intel_hasvk,asahi,broadcom,freedreno,panfrost,imagination,nouveau,virtio,microsoft-experimental '
                                   '-Dvulkan-layers=device-select,anti-lag -Dgles1=enabled -Dgles2=enabled '
                                   '-Dopengl=true -Dgbm=enabled -Dglx=dri -Degl=enabled -Dglvnd=enabled '
                                   '-Dintel-rt=enabled -Dmicrosoft-clc=disabled -Dllvm=enabled '
                                   '-Dshared-llvm=enabled -Dvalgrind=enabled -Dbuild-tests=false '
                                   '-Dandroid-libbacktrace=disabled -Dspirv-tools=enabled',
            'intel_virtio_define_count': 0,
            'icd_sha256': '10748bae30dc2449950a555a828632102543569d23fcb3231a58ad46947fa239',
            'icd_virtio_string_count': 0,
            'option_url': 'https://gitlab.freedesktop.org/mesa/mesa/-/raw/mesa-26.2.7/meson.options',
            'option_default': 'false'},
 '26.3.1': {'build_log_url': 'https://kojipkgs.fedoraproject.org/packages/mesa/26.3.1/1.fc46/data/logs/x86_64/build.log',
            'build_log_sha256': '40c26fa4c654cbeabb9e7d35ab92dd4fac6b8cfa09b325dce9e19c814404a96a',
            'meson_setup_line': 190,
            'meson_setup_command': '+ /usr/bin/meson setup --buildtype=plain --prefix=/usr '
                                   '--libdir=/usr/lib64 --libexecdir=/usr/libexec --bindir=/usr/bin '
                                   '--sbindir=/usr/bin --includedir=/usr/include --datadir=/usr/share '
                                   '--mandir=/usr/share/man --infodir=/usr/share/info '
                                   '--localedir=/usr/share/locale --sysconfdir=/etc --localstatedir=/var '
                                   '--sharedstatedir=/var/lib --wrap-mode=nodownload --auto-features=enabled '
                                   '. redhat-linux-build -Dplatforms=x11,wayland '
                                   '-Dgallium-drivers=llvmpipe,virgl,nouveau,r300,crocus,i915,iris,svga,radeonsi,r600,asahi,freedreno,etnaviv,tegra,vc4,v3d,lima,panfrost,zink,d3d12,ethosu,rocket '
                                   '-Dgallium-va=enabled -Dgallium-mediafoundation=disabled -Dteflon=true '
                                   '-Dgallium-rusticl=true '
                                   '-Dvulkan-drivers=swrast,amd,intel,intel_hasvk,asahi,broadcom,freedreno,panfrost,imagination,nouveau,virtio,gfxstream,microsoft-experimental '
                                   '-Dvulkan-layers=device-select -Dgles1=enabled -Dgles2=enabled '
                                   '-Dopengl=true -Dgbm=enabled -Dglx=dri -Degl=enabled -Dglvnd=enabled '
                                   '-Dintel-rt=enabled -Dmicrosoft-clc=disabled -Dllvm=enabled '
                                   '-Dshared-llvm=enabled -Dvalgrind=enabled -Dbuild-tests=false '
                                   '-Dandroid-libbacktrace=disabled -Dspirv-tools=enabled',
            'intel_virtio_define_count': 0,
            'icd_sha256': '5998cd7d42fc04c7a5f08c846bc3ffdd7e0460dab1188f6a6b3ad7390976a65c',
            'icd_virtio_string_count': 0,
            'option_url': 'https://gitlab.freedesktop.org/mesa/mesa/-/raw/mesa-26.3.1/meson.options',
            'option_default': 'false'}}
SCENE_HASH = "47972063c7a47bfb"
DEVICES = {
    "virgl": "virtio-vga-gl",
    "venus": "virtio-vga-gl,hostmem=4G,blob=true,venus=true",
    "native": "virtio-vga-gl,hostmem=4G,blob=true,venus=true,drm_native_context=on",
}


class Blocked(Exception):
    pass


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def capture(command, timeout=20, env=None):
    try:
        run = subprocess.run(command, capture_output=True, text=True, timeout=timeout, env=env)
        return {"exit_code": run.returncode, "stdout": run.stdout, "stderr": run.stderr}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"exit_code": None, "stdout": "", "stderr": str(error)}


def host_info(args):
    gpus = []
    for node in sorted(Path("/sys/class/drm").glob("renderD*")):
        dev = node / "device"
        entry = {"render_node": "/dev/dri/" + node.name, "pci_address": dev.resolve().name}
        for name in ("vendor", "device", "revision"):
            file = dev / name
            entry[name] = file.read_text().strip() if file.exists() else None
        entry["driver"] = (dev / "driver").resolve().name
        module = dev / "driver/module"
        for name in ("version", "srcversion"):
            file = module / name
            entry["module_" + name] = file.read_text().strip() if file.exists() else None
        gpus.append(entry)
    package_commands = [
        ["pacman", "-Q", "mesa", "virglrenderer", "nvidia-utils", "qemu-system-x86"],
        ["dpkg-query", "-W", "libegl-mesa0", "libvirglrenderer1", "qemu-system-x86"],
        ["rpm", "-q", "mesa-dri-drivers", "virglrenderer", "qemu-system-x86"],
    ]
    packages = [capture(c) for c in package_commands if shutil.which(c[0])]
    env = os.environ.copy()
    env["VK_DRIVER_FILES"] = args.vk_driver
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    runtime = {}
    if shutil.which("vulkaninfo") and args.vk_driver:
        runtime["vulkan"] = capture(["vulkaninfo", "--summary"], env=env)
    if shutil.which("eglinfo"):
        runtime["egl"] = capture(["eglinfo", "-B", "-p", "surfaceless"], env=env)
    if shutil.which("nvidia-smi") and any("nvidia" in p.lower() for p in args.vk_driver.split(":")):
        runtime["nvidia"] = capture(["nvidia-smi"], env=env)
    return {
        "kernel": platform.release(), "architecture": platform.machine(),
        "qemu": capture([args.qemu, "--version"]),
        "device_help": capture([args.qemu, "-device", "virtio-vga-gl,help"]),
        "display_help": capture([args.qemu, "-display", "help"]),
        "packages": packages, "gpus": gpus, "runtime": runtime,
        "vk_driver_files": args.vk_driver, "egl_render_node": args.render_node,
        "icd_sha256": {p: sha256(Path(p)) for p in args.vk_driver.split(":") if Path(p).is_file()},
    }


def preflight(args, host):
    release = re.match(r"(\d+)\.(\d+)", host["kernel"])
    if platform.system() != "Linux" or not release or tuple(map(int, release.groups())) < (6, 13):
        raise Blocked("The host needs Linux 6.13 or newer.")
    if host["architecture"] != "x86_64":
        raise Blocked("The pinned reference guest is amd64 and needs an x86_64 KVM host.")
    props = host["device_help"]["stdout"] + host["device_help"]["stderr"]
    if not re.search(r"\bvenus=", props):
        raise Blocked("QEMU does not expose the venus property on virtio-vga-gl.")
    if "egl-headless" not in host["display_help"]["stdout"]:
        raise Blocked("QEMU has no egl-headless OpenGL display backend.")
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise Blocked("KVM is unavailable or inaccessible.")
    if not os.access(args.render_node, os.R_OK | os.W_OK):
        raise Blocked("The selected EGL render node is inaccessible.")
    if not args.vk_driver or not all(Path(p).is_file() for p in args.vk_driver.split(":")):
        raise Blocked("Select an existing Vulkan ICD with --vk-driver or VK_DRIVER_FILES.")
    nv = host["runtime"].get("nvidia", {})
    if "Driver/library version mismatch" in nv.get("stdout", "") + nv.get("stderr", ""):
        raise Blocked("The NVIDIA kernel module and user-space driver versions differ; see host.gpus, packages and runtime.nvidia.")
    vk = host["runtime"].get("vulkan")
    if vk and vk["exit_code"] != 0:
        raise Blocked("The selected host Vulkan ICD cannot create an instance; see host.runtime.vulkan.")


def fetch(cache, digest, archive_path):
    target = cache / Path(archive_path).name
    if target.exists():
        if sha256(target) != digest:
            raise RuntimeError(f"Cached file fails SHA-256: {target}")
        return target
    partial = target.with_name(target.name + f".{os.getpid()}.partial")
    url = KOJI + archive_path
    failures = []
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=90) as response, partial.open("wb") as out:
                shutil.copyfileobj(response, out)
            if sha256(partial) != digest:
                raise RuntimeError(f"Download fails pinned SHA-256: {url}")
            partial.replace(target)
            return target
        except (OSError, urllib.error.URLError) as error:
            failures.append(str(error))
            time.sleep(attempt + 1)
        finally:
            partial.unlink(missing_ok=True)
    raise RuntimeError(f"Cannot download {archive_path}: {'; '.join(failures)}")


@contextlib.contextmanager
def rpm_tar(path):
    """Use the host's libarchive to convert RPM data into an in-memory tar.

    The @archive input copies archive members. It neither extracts them on
    the host nor runs RPM scriptlets or any program from the package.
    """
    run = subprocess.run(["bsdtar", "-cf", "-", "--format=pax", "@" + str(path)],
                         capture_output=True, check=True)
    with tarfile.open(fileobj=io.BytesIO(run.stdout), mode="r:") as archive:
        groups = {}
        for member in archive:
            if member.islnk():
                groups.setdefault(member.linkname, [member.linkname]).append(member.name)
        contents = {}
        for target, names in groups.items():
            if archive.getmember(target).size == 0:
                # RPM's cpio can carry a hard-link group's data on its last
                # member. A tar conversion makes that member a data-less link.
                # Read the group from the original archive to retain its bytes.
                contents[target] = subprocess.run(["bsdtar", "-xOf", str(path), "--", *names],
                                                  capture_output=True, check=True).stdout

        def read(member):
            target = member.linkname if member.islnk() else member.name
            return contents[target] if target in contents else archive.extractfile(member).read()

        yield archive, read


def cpio_entry(out, inode, name, mode, data=b"", rdev=(0, 0), nlink=1):
    encoded = name.encode() + b"\0"
    fields = [inode, mode, 0, 0, nlink, 0, len(data), 0, 0, *rdev, len(encoded), 0]
    header = b"070701" + b"".join(f"{value:08x}".encode() for value in fields)
    out.write(header + encoded)
    out.write(b"\0" * (-(len(header) + len(encoded)) % 4))
    out.write(data)
    out.write(b"\0" * (-len(data) % 4))


def check_assembled(build, expected, names):
    """A cached guest is reused only when its manifest covers exactly the files the boot reads, and they match."""
    if not isinstance(expected, dict) or set(expected) != set(names):
        raise RuntimeError(f"Assembled guest cache has an unexpected manifest: {build}")
    if any(sha256(build / name) != expected[name] for name in names):
        raise RuntimeError(f"Assembled guest cache fails SHA-256: {build}")


def assemble(cache, packages):
    """Create a root filesystem as newc initramfs, without host mounts or root."""
    identity = hashlib.sha256()
    for package in packages:
        identity.update(sha256(package).encode())
    for source in sorted(SOURCES.iterdir()):
        if source.is_file():
            identity.update(source.name.encode() + source.read_bytes())
    identity.update(Path(__file__).read_bytes())
    build = cache / identity.hexdigest()[:20]
    build.mkdir(exist_ok=True)
    kernel, initrd = build / "vmlinuz", build / "rootfs.cpio.gz"
    manifest = build / "artifacts.json"
    if kernel.exists() and initrd.exists() and manifest.exists():
        check_assembled(build, json.loads(manifest.read_text()), (kernel.name, initrd.name))
        return kernel, initrd
    files = {}
    modules = {}
    for package in packages:
        with rpm_tar(package) as (archive, read):
            for member in archive:
                name = str(PurePosixPath(member.name))
                if name == ".":
                    continue
                if name.startswith("/") or ".." in PurePosixPath(name).parts:
                    raise ValueError(f"Unsafe archive path: {name}")
                # RPMs can still name /lib64 while Fedora's filesystem aliases
                # it into /usr. The kernel's cpio unpacker does not traverse
                # those parent symlinks, so emit every child at its real path.
                if name.split("/", 1)[0] in ("bin", "sbin", "lib", "lib64"):
                    name = "usr/" + name
                if name.startswith("boot/vmlinuz-") or name.endswith("/vmlinuz"):
                    partial_kernel = kernel.with_suffix(f".{os.getpid()}.partial")
                    partial_kernel.write_bytes(read(member))
                    partial_kernel.replace(kernel)
                    continue
                if "/modules/" in name:
                    if re.search(r"\.ko(?:\.(?:xz|zst))?$", name):
                        key = Path(name).name.split(".ko")[0].replace("-", "_")
                        modules[key] = (name, read(member))
                    continue
                if name.startswith(("boot/", "usr/share/man/", "usr/share/locale/")):
                    continue
                if name.startswith("usr/share/doc/") and not name.endswith(("/copyright", "/LICENSE", "/COPYING")):
                    continue
                # No package privilege bits are needed by this one-shot root init.
                mode = member.mode & 0o777
                if member.isdir():
                    files[name] = (stat.S_IFDIR | mode, b"")
                elif member.issym():
                    files[name] = (stat.S_IFLNK | 0o777, member.linkname.encode())
                elif member.isfile() or member.islnk():
                    files[name] = (stat.S_IFREG | mode, read(member))
    order = []
    visited = set()

    def module(name):
        name = name.replace("-", "_")
        if name in visited or name not in modules:
            return
        visited.add(name)
        path, data = modules[name]
        if path.endswith(".xz"):
            data = lzma.decompress(data)
        elif path.endswith(".zst"):
            data = subprocess.run(["zstd", "-d", "-c"], input=data, capture_output=True, check=True).stdout
        match = re.search(rb"\x00depends=([^\x00]*)\x00", data)
        if match:
            for dependency in match[1].decode().split(","):
                if dependency:
                    module(dependency)
        dest = "modules/" + name + ".ko"
        files[dest] = (stat.S_IFREG | 0o644, data)
        order.append("/" + dest)

    module("virtio_pci")
    module("virtio_gpu")
    if not any("virtio_gpu" in path for path in order):
        raise RuntimeError("Pinned kernel archive lacks virtio_gpu")
    for directory in ("dev", "proc", "sys", "tmp", "run", "probe", "modules", "etc", "usr", "usr/bin"):
        files[directory] = (stat.S_IFDIR | 0o755, b"")
    for name, target in (("bin", "usr/bin"), ("sbin", "usr/sbin"), ("lib", "usr/lib"), ("lib64", "usr/lib64"),
                         ("usr/bin/sh", "busybox")):
        files[name] = (stat.S_IFLNK | 0o777, target.encode())
    for source in SOURCES.iterdir():
        if source.is_file():
            files["probe/" + source.name] = (stat.S_IFREG | 0o644, source.read_bytes())
    files["probe/modules.json"] = (stat.S_IFREG | 0o644, json.dumps(order).encode())
    # Retain a static PID 1 if the dynamic loader cannot start Python, so its
    # diagnostic reaches the serial log instead of being lost in a panic.
    init = "#!/bin/sh\n/usr/bin/python3 /probe/guest.py\n/bin/busybox sleep 1\n/bin/busybox poweroff -f\n"
    files["init"] = (stat.S_IFREG | 0o755, init.encode())
    # Directory records precede children, including the merged-/usr aliases.
    for name in list(files):
        for parent in PurePosixPath(name).parents:
            if str(parent) != ".":
                files.setdefault(str(parent), (stat.S_IFDIR | 0o755, b""))
    partial = initrd.with_suffix(f".{os.getpid()}.partial")
    with partial.open("wb") as raw, gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0, compresslevel=1) as out:
        for number, name in enumerate(sorted(files, key=lambda p: (p.count("/"), p)), 2):
            mode, data = files[name]
            cpio_entry(out, number, name, mode, data)
        cpio_entry(out, len(files) + 2, "dev/console", stat.S_IFCHR | 0o600, rdev=(5, 1))
        cpio_entry(out, len(files) + 3, "TRAILER!!!", 0)
    partial.replace(initrd)
    if not kernel.exists():
        raise RuntimeError("Pinned kernel archive has no vmlinuz")
    partial_manifest = manifest.with_suffix(f".{os.getpid()}.partial")
    partial_manifest.write_text(json.dumps({kernel.name: sha256(kernel), initrd.name: sha256(initrd)}) + "\n")
    partial_manifest.replace(manifest)
    return kernel, initrd


def run_guest(args, kernel, initrd, name, directory):
    directory.mkdir()
    trace = directory / "qemu.trace"
    serial = directory / "serial.log"
    cpu = Path("/proc/cpuinfo").read_text()
    accel = "kvm,honor-guest-pat=on" if "GenuineIntel" in cpu else "kvm"
    command = [args.qemu, "-machine", "q35,memory-backend=ram", "-accel", accel,
               "-cpu", "host", "-smp", "4", "-m", "4G", "-object",
               "memory-backend-memfd,id=ram,size=4G,share=on", "-nodefaults", "-no-reboot",
               "-snapshot", "-nic", "none", "-device", DEVICES[name],
               "-display", f"egl-headless,rendernode={args.render_node}",
               "-kernel", str(kernel), "-initrd", str(initrd),
               "-append", f"console=ttyS0 panic=-1 rdinit=/init gpu_reference={name}",
               "-serial", f"file:{serial}", "-monitor", "none", "-trace", "virtio_gpu_*", "-D", str(trace)]
    (directory / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    env = os.environ.copy()
    env["VK_DRIVER_FILES"] = args.vk_driver
    timed_out = False
    with (directory / "qemu.log").open("w") as log:
        process = subprocess.Popen(command, stdout=log, stderr=log, env=env, start_new_session=True)
        try:
            process.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
    text = serial.read_text(errors="replace") if serial.exists() else ""
    result = {"device": DEVICES[name], "exit_code": process.returncode, "timed_out": timed_out,
              "directory": str(directory.relative_to(args.out)), "status": "FAIL"}
    qemu_log = (directory / "qemu.log").read_text(errors="replace")
    if any(message in qemu_log for message in ("does not have OpenGL support", "eglInitialize failed",
                                               "render node init failed", "Property 'virtio-vga-gl.venus' not found")):
        result.update(status="BLOCKED", error="QEMU could not initialize the requested OpenGL/Venus backend; see qemu.log.")
        return result
    matches = re.findall(r"^GPU_REFERENCE_JSON=(.*)$", text, re.M)
    if matches:
        try:
            result["guest"] = json.loads(matches[-1])
            result["status"] = result["guest"]["status"] if process.returncode == 0 and not timed_out else "FAIL"
        except json.JSONDecodeError as error:
            result["error"] = f"Invalid guest serial record: {error}"
    else:
        result["error"] = "Guest did not emit its complete JSON record; inspect serial.log and qemu.log."
    trace_text = trace.read_text(errors="replace") if trace.exists() else ""
    events = {}
    for event in re.findall(r"\b(virtio_gpu_\w+)\b", trace_text):
        events[event] = events.get(event, 0) + 1
    result["trace"] = {"events": events, "sha256": sha256(trace) if trace.exists() else None}
    if not events.get("virtio_gpu_cmd_ctx_create") or "virtio_gpu_features virgl 1" not in trace_text:
        result["status"] = "FAIL"
        result["trace"]["error"] = "No host proof of virgl negotiation and context creation."
    if name != "virgl" and not all(events.get(event) for event in ("virtio_gpu_cmd_ctx_submit",
                                                                  "virtio_gpu_cmd_res_create_blob",
                                                                  "virtio_gpu_cmd_res_map_blob")):
        result["status"] = "FAIL"
        result["trace"]["error"] = "No host proof of submissions and mapped blobs on the Venus device."
    return result


def scene_exact(scene):
    return (isinstance(scene, dict) and scene.get("hash") == SCENE_HASH
            and scene.get("differing_pixels") == 0 and scene.get("triangle_pixels") == 17540)


def repeatability(runs):
    """Compare scene hashes and complete capset bytes, not timing-dependent logs.

    A device repeats only when every run passed, every scene that ran is exact, and the runs agree.
    A scene recorded as unavailable (native contexts in a stock build) is compared but never required."""
    groups = {}
    for run in runs:
        guest = run.get("guest", {})
        scenes = {g["name"]: "UNAVAILABLE" if g.get("status") == "UNAVAILABLE" else g.get("scene")
                  for g in guest.get("gl", [])}
        groups.setdefault(run["device"], []).append(
            (run.get("status"), {"capset": guest.get("venus_capset"), "scenes": scenes}))
    summary = {}
    for device, values in groups.items():
        statuses = {status for status, _ in values}
        prints = [fingerprint for _, fingerprint in values]
        if statuses == {"BLOCKED"}:
            status = "BLOCKED"
        elif statuses != {"PASS"}:
            status = "FAIL"
        elif not all(p["scenes"] and all(s == "UNAVAILABLE" or scene_exact(s) for s in p["scenes"].values())
                     for p in prints):
            status = "FAIL"
        elif len(prints) == 1:
            status = "NOT_RUN"
        else:
            status = "PASS" if all(p == prints[0] for p in prints) else "FAIL"
        summary[device] = {"runs": len(values), "status": status}
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__ or "Pinned Linux GPU reference for FerrionOS (no host root).")
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    parser.add_argument("--vk-driver", default=os.environ.get("VK_DRIVER_FILES", ""), help="Host Vulkan ICD JSON, or colon-separated ICD files")
    parser.add_argument("--render-node", default="/dev/dri/renderD128", help="Host EGL render node (recorded independently of the Vulkan ICD)")
    parser.add_argument("--out", type=Path, default=ROOT / "build/gpu-reference")
    parser.add_argument("--cache", type=Path, default=ROOT / "build/gpu-reference/cache/fedora-koji")
    parser.add_argument("--repeat", type=int, default=2, help="Boots per device line; default 2 checks repeatability")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--fetch-only", action="store_true", help="Verify/download pinned inputs without booting")
    parser.add_argument("--variant", choices=("all", "fedora45", "mesa263"), default="fedora45",
                        help="Fedora 45 (default), Fedora 45 with rawhide Mesa 26.3.1, or both")
    args = parser.parse_args()
    if not args.fetch_only and not args.vk_driver:
        parser.error("choose --vk-driver or set VK_DRIVER_FILES; use --fetch-only to prepare the cache")
    if args.repeat < 1 or args.timeout < 1:
        parser.error("--repeat and --timeout must be positive")
    args.out = args.out.resolve()
    args.cache = args.cache.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    args.cache.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    run_dir = args.out / f"run-{stamp}-{os.getpid()}"
    run_dir.mkdir()
    variants = [("fedora45", {"mesa": MESA, "glibc": "2.44-2.fc45", "llvm": "23.1.5-2.fc45",
                              "kind": "Fedora 45 release and updates"}),
                ("mesa263", {"mesa": CURRENT_MESA, "glibc": "2.44.9000-7.fc46", "llvm": "23.1.5-2.fc45",
                             "kind": "Fedora 45 with rawhide Mesa and its minimal runtime overlay"})]
    record = {"schema_version": 3, "status": "FAIL", "started_utc": stamp,
              "base_pin": {"distribution": "Fedora 45", "kernel": KERNEL},
              "trust": {"method": "RPM header signatures and signed payload digests verified when pins were established",
                        "key_fingerprints": FEDORA_KEYS, "archive": KOJI},
              "expected_scene_hash": SCENE_HASH,
              "variants": [{"name": name, "pin": pin, "runs": [], "status": "NOT_RUN"}
                           for name, pin in variants if args.variant in ("all", name)]}
    try:
        record["host"] = host_info(args)
        if not args.fetch_only:
            preflight(args, record["host"])
            if not all(shutil.which(tool) for tool in ("bsdtar", "zstd")):
                raise Blocked("The host needs bsdtar and zstd to read the pinned RPMs and kernel modules.")
        pins = [line.split() for line in PACKAGE_PINS.splitlines() if line.strip()]
        print(f"Checking {len(pins)} pinned Fedora 45 packages", flush=True)
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            packages = list(pool.map(lambda pair: fetch(args.cache, *pair), pins))
        record["base_inputs"] = [{"filename": p.name, "sha256": digest} for p, (digest, _) in zip(packages, pins)]
        for variant in record["variants"]:
            variant_name = variant["name"]
            variant["status"] = "FAIL"
            variant_packages = packages
            if variant_name == "mesa263":
                overlay_pins = [line.split() for line in CURRENT_PINS.splitlines() if line.strip()]
                overlay_cache = args.cache
                overlay_cache.mkdir(exist_ok=True)
                print(f"Checking {len(overlay_pins)} pinned Mesa runtime overlay packages", flush=True)
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                    overlay = list(pool.map(lambda pair: fetch(overlay_cache, *pair), overlay_pins))
                variant["overlay_inputs"] = [{"filename": p.name, "sha256": digest} for p, (digest, _) in zip(overlay, overlay_pins)]
                variant_packages = packages + overlay
            if args.fetch_only:
                variant["status"] = "FETCHED"
                continue
            print(f"Assembling the offline {variant_name} guest", flush=True)
            kernel, initrd = assemble(args.cache, variant_packages)
            variant["artifacts"] = {"kernel_sha256": sha256(kernel), "rootfs_sha256": sha256(initrd)}
            for name in DEVICES:
                for repeat in range(args.repeat):
                    print(f"Booting {variant_name}/{name}, run {repeat + 1}/{args.repeat}", flush=True)
                    result = run_guest(args, kernel, initrd, name, run_dir / f"{variant_name}-{name}-{repeat + 1}")
                    variant["runs"].append(result)
                    print(f"{result['status']} {variant_name}/{name}", flush=True)
            native_runs = [r["guest"] for r in variant["runs"] if "native_context" in r.get("guest", {})]
            if native_runs:
                variant["native"] = {
                    "status": "available" if all(g["native_context"]["status"] == "AVAILABLE" for g in native_runs) else "unavailable",
                    "reason": native_runs[0]["native_context"].get("reason"),
                    "evidence": NATIVE_EVIDENCE[variant["pin"]["mesa"].split("-", 1)[0]],
                }
            variant["repeatability"] = repeatability(variant["runs"])
            statuses = [r["status"] for r in variant["runs"]] + [r["status"] for r in variant["repeatability"].values()]
            variant["status"] = "FAIL" if "FAIL" in statuses else "BLOCKED" if "BLOCKED" in statuses else "PASS"
        statuses = [variant["status"] for variant in record["variants"]]
        record["status"] = "FAIL" if "FAIL" in statuses else "BLOCKED" if "BLOCKED" in statuses else "FETCHED" if args.fetch_only else "PASS"
    except Blocked as error:
        record.update(status="BLOCKED", error=str(error))
        for variant in record["variants"]:
            if variant["status"] == "NOT_RUN":
                variant.update(status="BLOCKED", error=str(error))
    except (OSError, ValueError, RuntimeError, tarfile.TarError, subprocess.SubprocessError) as error:
        record["error"] = str(error)
    except KeyboardInterrupt:
        record["error"] = "Interrupted; the guest process group was stopped."
    destination = run_dir / "result.json"
    destination.write_text(json.dumps(record, indent=2) + "\n")
    print(f"{record['status']}: {destination}")
    if "error" in record:
        print(record["error"], file=sys.stderr)
    return {"PASS": 0, "FETCHED": 0, "BLOCKED": 2}.get(record["status"], 1)


if __name__ == "__main__":
    sys.exit(main())
