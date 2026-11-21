#!/usr/bin/env python3
# Build and boot an offline, pinned Linux GPU reference for FerrionOS.
# Downloads are data on the host; all downloaded programs run inside QEMU.

import argparse
import concurrent.futures
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
SNAPSHOT = "20260913T000000Z"
INDEX_SHA256 = "7778d3e3f303b7ddb8ce0fe7c8d57473a076c6bf2e8f241f75421d2396352498"
RELEASE_SHA256 = "0584fba32e13e0ab8285fb16c27adea1ec03a73669c18702821094fd6ca86675"
KERNEL = "6.12.107+deb13-amd64"
MESA = "25.0.7-2+deb13u1"
CURRENT_SNAPSHOT = "20261121T000000Z"
CURRENT_INDEX_SHA256 = "5fbf6c2463072d01ea1f26b346aca503bfc0e4a5ec8ff7c6bfd6d6000d9d7b15"
CURRENT_RELEASE_SHA256 = "6e8a85389997ac42de32f929b69157a53bb230f183ff0ddbf645ec70010374e4"
CURRENT_MESA = "26.2.6-1"
CURRENT_GLIBC = "2.43-7"
CURRENT_LLVM = "1:22.1.8-4"
# Minimal runtime upgrades required by the six Mesa packages, from the signed
# unstable index above. The kernel and compiler remain from the Debian 13 pin.
CURRENT_PINS = """
8abae40b5e2bfca58fbc838329b48f525c98463e930b8d3ed075d0c9e960a57a pool/main/g/glibc/libc-gconv-modules-extra_2.43-7_amd64.deb
5c71715c51103beb4fbba9fe9f8dff604af6610dfa71c70bff342f54a7860803 pool/main/g/glibc/libc6_2.43-7_amd64.deb
dfc160ad29f280ec15fea4d3d61536d2b477b37288b53276bbd121a185d801d6 pool/main/libd/libdisplay-info/libdisplay-info3_0.3.0-1+b1_amd64.deb
b7282a3422545c6cb488c3f4f5740844ad06c31030e7b9600700342a807ff519 pool/main/libd/libdrm/libdrm-amdgpu1_2.4.134-3_amd64.deb
89127ee70fd643f0f1c1fe294bfda51315ea4e92234500211f9956351706363f pool/main/libd/libdrm/libdrm-common_2.4.134-3_all.deb
f9ebd503a2e98d26a2936a4c875370260531a702e2c44aa5d6348a4c2b212b4b pool/main/libd/libdrm/libdrm-intel1_2.4.134-3_amd64.deb
e1a841ed70c7b7bbd9a03f41c781101d668db0d35b4eee58d7705a6c32b88aaa pool/main/libd/libdrm/libdrm2_2.4.134-3_amd64.deb
5ae16aef5adc19aad87dcd4e0b19010a5802d4cc6e5f826e8990ad3dbca0105d pool/main/m/mesa/libegl-mesa0_26.2.6-1_amd64.deb
fdd7d975841d9d73380649f4bb5840073e38435701af9a97d5cc57c871d93daa pool/main/m/mesa/libgbm1_26.2.6-1_amd64.deb
5ccd9e530adb132ad91cf768ea186c523e6dcef4f7245bac935091469912fa60 pool/main/m/mesa/libgl1-mesa-dri_26.2.6-1_amd64.deb
85a66b48749c68b8d28ac2dc1282a71d35bfcc510c0ee42bb2c9e9ffe197df80 pool/main/m/mesa/libglx-mesa0_26.2.6-1_amd64.deb
3bf498e61dbbef0cfd6bbdb97f4530f96f59806739d5964733d8a55e93d68f89 pool/main/l/llvm-toolchain-22/libllvm22_22.1.8-4_amd64.deb
fe67acf1d02f21062c0b75cfe3299191b38967d89cfd5cab3a831e2e4b695d05 pool/main/libp/libpng1.6/libpng16-16t64_1.6.59-1_amd64.deb
b432b1d30319f371efe776209e36143a23289285dd26d41751e89ad4a88b2c33 pool/main/w/wayland/libwayland-client0_1.26.0-1_amd64.deb
f2b4d9acff743ff71f5409ae301452ebbb37d0d8709cf46fc9ec755d7df803ff pool/main/libx/libxml2/libxml2-16_2.15.4+dfsg-1+b1_amd64.deb
d7819525795c50645f4dd08fa53c75258a6ae433bb7ddaa13059af9bb609abfe pool/main/m/mesa/mesa-libgallium_26.2.6-1_amd64.deb
fe78bf25497ad1a7afae87cc59838fe86344d811e2e87d6f8047612fae521711 pool/main/m/mesa/mesa-vulkan-drivers_26.2.6-1_amd64.deb
"""
SCENE_HASH = "47972063c7a47bfb"
# From the signed trixie main/binary-amd64 index above, including the compiler
# used inside the guest. No package maintainer scripts execute on the host.
PACKAGE_PINS = """
6bc08c02539ba53b5e748142397144c499f9b20b5fa9bb56431545db124addeb pool/main/b/binutils/binutils_2.44-3_amd64.deb
002da5d23f8757dee97a2c0a40e0e1d4d85a43da094488ee2ee7068d4d3691f9 pool/main/b/binutils/binutils-common_2.44-3_amd64.deb
e6741ce95ff0f7a131c8d9faa3528ccbbc453078bbc62a97da81340ed7462c53 pool/main/b/binutils/binutils-x86-64-linux-gnu_2.44-3_amd64.deb
598a3fd92bdafc34cd81196b2952ad36e910e47a4ecf162f8ce5e8e262598e53 pool/main/b/busybox/busybox-static_1.37.0-6+b9_amd64.deb
a6ba40550aadec80437813e0fce5516574945ff40e72306a5b378c03af3a6544 pool/main/g/gcc-defaults/cpp_14.2.0-1_amd64.deb
3bae5fe03c56c6d021900ab4fb79e35c6f75eee72a73fccc6f28a39db559d6a0 pool/main/g/gcc-14/cpp-14_14.2.0-19_amd64.deb
ef274b5379f5f97fc71619d39ecc84d039d3e184570d207b909c90afaa5d79e0 pool/main/g/gcc-14/cpp-14-x86-64-linux-gnu_14.2.0-19_amd64.deb
d902f484e38bce59e82efc0df2fe20139a0932ca759ecafdfe6e5b6c72238eb8 pool/main/g/gcc-defaults/cpp-x86-64-linux-gnu_14.2.0-1_amd64.deb
1ddc7e6c5925f748eefb7ca811d98e049f18c158bb21fbc7e122262ce6011e49 pool/main/d/debconf/debconf_1.5.91_all.deb
3e800c6d75e8e709007ed1c356e5fa8509a75c1694fb0a1183709abc9cfdc1f3 pool/main/d/dpkg/dpkg_1.22.22_amd64.deb
eee65c41a441c57c05f754ced1b9d093956966a1c7da9938c3ec00d3dc2f905d pool/main/g/gcc-defaults/gcc_14.2.0-1_amd64.deb
21500408b5019d8d29f70ce58488e2eea469a1adb4b5fd7db45e819b5efcac4e pool/main/g/gcc-14/gcc-14_14.2.0-19_amd64.deb
5b6825de4263824b78c4c51f6476414f3b4e89c2ab63e81dc8b9b5501e867cf6 pool/main/g/gcc-14/gcc-14-base_14.2.0-19_amd64.deb
a17ef039f1ba482051c3efb5c2c24070002e60dd0bd09fd456ec31481a11b725 pool/main/g/gcc-14/gcc-14-x86-64-linux-gnu_14.2.0-19_amd64.deb
04c28c2760b0094b3d7758154ac095303dd00599b63ebad4086a325b607308b7 pool/main/g/gcc-defaults/gcc-x86-64-linux-gnu_14.2.0-1_amd64.deb
08074f01e384bc07c0c2d79a58cf4a6523f71cf75d1808101c79617656c9a39d pool/main/a/acl/libacl1_2.3.2-2+b1_amd64.deb
771899ade7310fe31aa538a509354c33d62bbe4e408b4e5fc1e513988dc42645 pool/main/g/gcc-14/libasan8_14.2.0-19_amd64.deb
212b399aae2f7299203d261a57e49372e09565a9a5ea971905f94a3960366c05 pool/main/g/gcc-14/libatomic1_14.2.0-19_amd64.deb
4f4664c8a8f0ad0c8631c39fab02e3d8d86ccc6f4436a1d59f059dbcb0492679 pool/main/b/binutils/libbinutils_2.44-3_amd64.deb
e5a85986fa6bec3307ab1bc860736b478b331882bc45e17675a7bdf88eecb43a pool/main/libb/libbsd/libbsd0_0.12.2-2_amd64.deb
cba4cda04244b5e481bb15524bc3c983a7d1b6f330013b9b381706a2fcb65310 pool/main/b/bzip2/libbz2-1.0_1.0.8-6_amd64.deb
2c21175d6a8283ed566d154c0c0bc06f60836e53b0b9e1f74c14eac586b68c24 pool/main/g/glibc/libc-dev-bin_2.41-12+deb13u4_amd64.deb
967aa62605721081c3eb2a17650611a792aa802d76a6511d1840242623d204c9 pool/main/g/glibc/libc6_2.41-12+deb13u4_amd64.deb
1fda734dabcd80b77266a09ab62b0f1e3e16d8091db62899890b2200745632a2 pool/main/g/glibc/libc6-dev_2.41-12+deb13u4_amd64.deb
2d587e3bfba4aec701946b7ba0c1640b97b6d8dd0a34ba1c70ae85261ecedefa pool/main/g/gcc-14/libcc1-0_14.2.0-19_amd64.deb
98e2333aea8d64ca68f9b75c16256d3a05492fd8f9e6dba96adbc6f19e4f5a09 pool/main/libx/libxcrypt/libcrypt-dev_4.4.38-1_amd64.deb
0ebc144d662e3197982d1bf3a7b8b35ca845e54c68811de0328b1f0d7c67585c pool/main/libx/libxcrypt/libcrypt1_4.4.38-1_amd64.deb
e280b2be3db6e584500e865c251605b95c346767d87ccb2524e44992048fc657 pool/main/b/binutils/libctf-nobfd0_2.44-3_amd64.deb
120cafcd93132a276fa92a8fb4cf39b23d14e5a3e348f4f5580638d71ca95ac5 pool/main/b/binutils/libctf0_2.44-3_amd64.deb
18d02510ee78b67e4504ba050176797a200ff24214a7cd318082ab60ad7bf3fc pool/main/d/db5.3/libdb5.3t64_5.3.28+dfsg2-9_amd64.deb
c1d97a5e32e2bc68e833b8abeae026b6306198e87ee4396a7ffecd4779caefbf pool/main/libd/libdrm/libdrm-amdgpu1_2.4.124-2_amd64.deb
9a8a6c65c165e9964f106fb4ac710959b5d33e0790227e3ab6b27c4742d1254a pool/main/libd/libdrm/libdrm-common_2.4.124-2_all.deb
83b84b1207c4dda3d64ebad7725a7ad237c69c870668db3b01eabad1052f0dcb pool/main/libd/libdrm/libdrm-dev_2.4.124-2_amd64.deb
188ab1fd74c838b3c8055d62107351e16a4bb7a25d39c30bbb9aac30ddd37238 pool/main/libd/libdrm/libdrm-intel1_2.4.124-2_amd64.deb
634dcb46487fb2ca267850261291463845ada90b81d16a926ca40d333ce57180 pool/main/libd/libdrm/libdrm-nouveau2_2.4.124-2_amd64.deb
7a82b1c88b45235e314a15b71781b47ed472dafbbf697481ec8427079840fc32 pool/main/libd/libdrm/libdrm-radeon1_2.4.124-2_amd64.deb
fe2276901c7cd7b8079de63072d37fe1cbeb4eb001a3bc1f1d662ad89aa0890e pool/main/libd/libdrm/libdrm2_2.4.124-2_amd64.deb
b002ea172b9c1e34a67bc497c523c67bb74c3f0a4e98113cb083990a1f1d3bfe pool/main/libe/libedit/libedit2_3.1-20250104-1_amd64.deb
97fcfff08a5de0a510066d55260a30fbd3ccbd87da6c2f317e6efe17566b9f5a pool/main/libg/libglvnd/libegl-dev_1.7.0-1+b2_amd64.deb
0ddebd7e91b048aac812611d6bd008343386cb140b51e5e66df74c6b6263be65 pool/main/m/mesa/libegl-mesa0_25.0.7-2+deb13u1_amd64.deb
62e7bc8ccd47a60e55aad17f83d8d474310d782eb8589bbce2ab714c31712406 pool/main/libg/libglvnd/libegl1_1.7.0-1+b2_amd64.deb
94497b7e17b6f574a0605b380d454e20d3f01a9c63b70c2a2263f679d30053e1 pool/main/e/elfutils/libelf1t64_0.192-4_amd64.deb
38abe0e710a07688e9c149d74536e67cfee0364bdb64dd6d644c32a1cfad389f pool/main/e/expat/libexpat1_2.8.3-1~deb13u1_amd64.deb
0ebdc340de33333639c3c63874cd4b15ac2e83dfa1ef3053b7eefaf4919f4f68 pool/main/libf/libffi/libffi8_3.4.8-2_amd64.deb
31fb6d76b9ceaf13848fa617df53f85f62626b4fe7464a93811c720af6d5f2dd pool/main/m/mesa/libgbm1_25.0.7-2+deb13u1_amd64.deb
ca6f2d36d96b19b3eb71405b0b80134d8c89380b02204a2512e5c58ceb090628 pool/main/g/gcc-14/libgcc-14-dev_14.2.0-19_amd64.deb
3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150 pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb
2c5044a44bc7cd7a196d554ca6ff78ae0358fbb98b7d2cce49176bdc6a816fe4 pool/main/libg/libglvnd/libgl-dev_1.7.0-1+b2_amd64.deb
87fa2f6e5abaed4ed385fac879c8dd735af719ee2300222d901793c66e041678 pool/main/libg/libglvnd/libgl1_1.7.0-1+b2_amd64.deb
722fad9944bdfd29eb6421576331a2697f83bc4b3551d693edb7aefdde6bb198 pool/main/m/mesa/libgl1-mesa-dri_25.0.7-2+deb13u1_amd64.deb
87af52464cb40d54e78ecf2046db019bf1ab19a8bed7511ab282be623d1216de pool/main/libg/libglvnd/libgles-dev_1.7.0-1+b2_amd64.deb
ef909f784ed99a849bfdf69abaa0747cf842abbb6d3f4f119481a2f9e3431284 pool/main/libg/libglvnd/libgles1_1.7.0-1+b2_amd64.deb
e12c18aa77939d122ee1dea520ba0440b97dc19200b7396e03dff03a3ece74c1 pool/main/libg/libglvnd/libgles2_1.7.0-1+b2_amd64.deb
887f74008166549ce9e100c906aa937e95d6e5ce1c8d86efe8c95fd953359b9c pool/main/libg/libglvnd/libglvnd0_1.7.0-1+b2_amd64.deb
93d456321c07009491b942783db8c57065b616db713ef5f6a0c629ca078c4ea0 pool/main/libg/libglvnd/libglx-dev_1.7.0-1+b2_amd64.deb
2e6ab57de9931e890621d3ade04213b06e7142a4ae8a72a1756c44d0271aaeb7 pool/main/m/mesa/libglx-mesa0_25.0.7-2+deb13u1_amd64.deb
2721fdca0fe3bd963cb39482eabc253af52b88f4a7f6dbb69e475549daf5af3b pool/main/libg/libglvnd/libglx0_1.7.0-1+b2_amd64.deb
d0d0265eb01770f17afd0f7c8c0622f80479dcfbbe13653a0debeec61464e622 pool/main/g/gmp/libgmp10_6.3.0+dfsg-3_amd64.deb
72b7a1a33c48f3195efeb044111b34ae684b1e640bb6495c832443b798601cd5 pool/main/g/gcc-14/libgomp1_14.2.0-19_amd64.deb
0d36d0bde85c467ac6735dded08e19dd30ae1369e3908b9eb48ccc0d6ff66648 pool/main/b/binutils/libgprofng0_2.44-3_amd64.deb
2cdaf797cad8ac30a964ab081370682dd065ae1b0305a0be9d8e0e14cd0657d7 pool/main/g/gcc-14/libhwasan0_14.2.0-19_amd64.deb
ac8518042e81c00de1effb72bba7e88ac4ecd488f7ea8b9e3ebc63159cb53b35 pool/main/i/isl/libisl23_0.27-1_amd64.deb
febdb3c528cde1a3abe952e17a1d3cfa7ccaca5b6f506d983d34385dad199490 pool/main/g/gcc-14/libitm1_14.2.0-19_amd64.deb
60707a62fe6c1228c3389b12a13ca4efd76defc5532473e547a29e99cf7d2a6e pool/main/j/jansson/libjansson4_2.14-2+b3_amd64.deb
db0d614d61345ca710fb73e75105f1ec6e38898fa7b3aa99fa7eb7d8b0a11d57 pool/main/l/llvm-toolchain-19/libllvm19_19.1.7-3+b1_amd64.deb
a0df07da76287a45e665d584363dc2a6a1c4e240aee41b8bbc0f67cdff97cd68 pool/main/g/gcc-14/liblsan0_14.2.0-19_amd64.deb
1cfcc6e0dc36f438a79b6e2189facdb9d150b08f57d190a60e01c98075c7f896 pool/main/x/xz-utils/liblzma5_5.8.1-1+deb13u1_amd64.deb
7244ec3839b61fac0c1884fe08aaa040f26e8f1f35f1f5d3482eacefd30d1b44 pool/main/libm/libmd/libmd0_1.1.0-2+b1_amd64.deb
2af0a5c128e03694a41c0b011bd8a958b7297436cdb3a15ddad7866dae8c300b pool/main/m/mpclib3/libmpc3_1.3.1-1+b3_amd64.deb
75dddce11dabc7fc543712c33dc27b7f2ee66a111763eb5eac654d010b42cd92 pool/main/m/mpfr4/libmpfr6_4.2.2-1_amd64.deb
47baa2e11579583654583a066b69bd5a5d2f22423081f707ed6c0c3ba6d538b7 pool/main/n/ncurses/libncursesw6_6.5+20250216-2_amd64.deb
dfaa687d4ccb943f5b222261a11a1678992bc499abe472d539aa59c4787a0c33 pool/main/libp/libpciaccess/libpciaccess-dev_0.17-3+b3_amd64.deb
d9a0091071635a84e837051e4813005ac445071731becea28fba4d9806df5252 pool/main/libp/libpciaccess/libpciaccess0_0.17-3+b3_amd64.deb
1252b96a5bc44bb5db982bef8eb18e54f5047cede2aff641bce4f8e1edb91c3e pool/main/p/pcre2/libpcre2-8-0_10.46-1~deb13u2_amd64.deb
7e142ed64a81ccdd39714f213467bd365736ef341adb2be11be981f638b9175f pool/main/p/python3-defaults/libpython3-stdlib_3.13.5-1_amd64.deb
a4aa6a9a8c77f87bfe545e1d14ec42c001d189ace90e3aa75deec882b39dee49 pool/main/p/python3.13/libpython3.13-minimal_3.13.5-2+deb13u5_amd64.deb
db161322a3481d2c0c3b9a3b9a03c3ab0e2b1718f54b88755fb4a3f939165b84 pool/main/p/python3.13/libpython3.13-stdlib_3.13.5-2+deb13u5_amd64.deb
f5b51cf0ece3ec3bb7494d0cdbff3ce715ae00cfb2b6ed787f7db91319d64a7b pool/main/g/gcc-14/libquadmath0_14.2.0-19_amd64.deb
eeadf2b5e755c9f183883feea2d9b5b28560284275d5f54d3e55d0923b1d0967 pool/main/r/readline/libreadline8t64_8.2-6_amd64.deb
68bb8d32bd8d6d7d2f5952a169db03d1484b46ae1e52abccdec42a19dccea5d5 pool/main/libs/libselinux/libselinux1_3.8.1-1_amd64.deb
3056da80c7d963af795dab480ab6f6f4b154ad4ac39f522dc52d17c834fea253 pool/main/l/lm-sensors/libsensors-config_3.6.2-2_all.deb
f0a994a6d7cfa695dea5343d0d1ba7eed796c0ad920c7282998b95f60049c4f6 pool/main/l/lm-sensors/libsensors5_3.6.2-2_amd64.deb
38f625dfdc582717029ac3a3e97c51d994ec2e7a0e9b230c6b44e40d1276311f pool/main/b/binutils/libsframe1_2.44-3_amd64.deb
0a459adaffd901109f7811ab65f58e7a957b4907d05539cf3d1184efdcde0468 pool/main/s/sqlite3/libsqlite3-0_3.46.1-7+deb13u2_amd64.deb
916f7f40b34a06e6ebfaefcdab331bff458328411da672598f126a760472467d pool/main/o/openssl/libssl3t64_3.5.7-1~deb13u2_amd64.deb
ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0 pool/main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb
8b9f6a7983e9418564e48a627518de4c03917b56efe68d7f3e93bd8fffa1cc10 pool/main/n/ncurses/libtinfo6_6.5+20250216-2_amd64.deb
529d7f68227d16975076c5f842e8b687faea1b5df8e0dcf62e436d5402844ca5 pool/main/g/gcc-14/libtsan2_14.2.0-19_amd64.deb
9205a1bef7d6a52ffc06548e0cde169ff303e688c12d9be3a98f734449077e22 pool/main/g/gcc-14/libubsan1_14.2.0-19_amd64.deb
c1bf4c4c3ff48c57fabf93307dfb56996b60cfa33927afc4158b5db36fb2721e pool/main/u/util-linux/libuuid1_2.41.5-0+deb13u1_amd64.deb
8d329d465e16168afff58b1bd0dd6db46aac2cc3f56054a481b25a04e90b345a pool/main/v/vulkan-loader/libvulkan-dev_1.4.309.0-1_amd64.deb
f47da79cd140264fe21cceb08bc87a71bc7fec05819e4a421f3f518d21101a37 pool/main/v/vulkan-loader/libvulkan1_1.4.309.0-1_amd64.deb
d1607e1db1a7c5378a2e5c6ae7b20f64dc6acf134f953d84a7f5204b74339f33 pool/main/w/wayland/libwayland-client0_1.23.1-3_amd64.deb
2967212bd582e0dffca443fdc44f4c660e7368d41f7ee3a7f6314e0c3abfe9ea pool/main/w/wayland/libwayland-server0_1.23.1-3_amd64.deb
b5a3fd3bf8c8fd0364bfb9bea00dcba7fc301229bd02dded084632d31f5b0fb3 pool/main/libx/libx11/libx11-6_1.8.12-1_amd64.deb
c54f87069888f80ba4da586da6147d74c7598ccdd8b90906dbc4271fa414c738 pool/main/libx/libx11/libx11-data_1.8.12-1_all.deb
530de60c9c6dadd997ae7d74eca2a41b25e849373a5ad5dcdb619aa05713d6e0 pool/main/libx/libx11/libx11-dev_1.8.12-1_amd64.deb
e05f94d21a932fba5b09b9b13d99df776b155d3bc792c0e294451df9ffe1ba25 pool/main/libx/libx11/libx11-xcb1_1.8.12-1_amd64.deb
1e29410af794a3d3790aab9fa0c1b0ec9324697502e982fce4dcdd4610ce3e8a pool/main/libx/libxau/libxau-dev_1.0.11-1_amd64.deb
689a9f0e0ba3e2c65431f864871e303ee904de69dd28abfc462663fae030227f pool/main/libx/libxau/libxau6_1.0.11-1_amd64.deb
f446d42fb5fcebbb3e347368ba83616769fdb271d85b2f49048e337f3163d267 pool/main/libx/libxcb/libxcb-dri3-0_1.17.0-2+b1_amd64.deb
cc59a3fa1c4ce376c3a32798179950810bd7b9be9a35bc76de1f5b5e7cc3925e pool/main/libx/libxcb/libxcb-glx0_1.17.0-2+b1_amd64.deb
db95ea4630c55bd7f3281cb60ccf75b1627ef2f4399e1939f8f6161e584a92fb pool/main/libx/libxcb/libxcb-present0_1.17.0-2+b1_amd64.deb
f5d9fe5fdf797918f81f0abf6cfd4270bd54659540cddb4da709ab7154523214 pool/main/libx/libxcb/libxcb-randr0_1.17.0-2+b1_amd64.deb
d1777a813f484e89f3dcaadd1d5f2b1a13e9313b6bf70eded16d8ce743208fef pool/main/libx/libxcb/libxcb-shm0_1.17.0-2+b1_amd64.deb
0ce4770ed1505be9ddc6473045e4662d062aff2f3077ee684265f01cfb543559 pool/main/libx/libxcb/libxcb-sync1_1.17.0-2+b1_amd64.deb
f9c1aafc18bf9e4662e34bbaf3bb00f1f4e8c2fc1dd786fdfb6cb9cb6c64fae7 pool/main/libx/libxcb/libxcb-xfixes0_1.17.0-2+b1_amd64.deb
5c222a72d11b866447da31693254f738430726e3e065a384e82687b2fd2f978b pool/main/libx/libxcb/libxcb1_1.17.0-2+b1_amd64.deb
669949f8d5dff76616630d839b9047b76af553c62c376458094bd759f8fdb903 pool/main/libx/libxcb/libxcb1-dev_1.17.0-2+b1_amd64.deb
bc845297d20cf63388c6b248d9ee169a3890347cb1c29bcd9f771684bf17b528 pool/main/libx/libxdmcp/libxdmcp-dev_1.1.5-1_amd64.deb
0740dc760916b2008b45417a42a8fd7dd5de370fb57d31373f15034cda8acf0b pool/main/libx/libxdmcp/libxdmcp6_1.1.5-1_amd64.deb
fc618ec40465e5ce48622606299cb47833efc3fb235ba15543b81f850722f443 pool/main/libx/libxext/libxext6_1.3.4-1+b3_amd64.deb
e0c6b63ce4602a036a526f60fe5e6c1586710688058d98fc1001b9b3147b7efd pool/main/libx/libxml2/libxml2_2.12.7+dfsg+really2.9.14-2.1+deb13u3_amd64.deb
7b339e9e5b2349723d35af4df89bcc7aa456bbdf8ba1754358f9b44c3fe1f964 pool/main/libx/libxshmfence/libxshmfence1_1.3.3-1_amd64.deb
f9f8487e536293e7ecf91fe7feb14e97e02a3aa652e207c661442dce775d3b14 pool/main/libx/libxxf86vm/libxxf86vm1_1.1.4-1+b4_amd64.deb
71383373523ef62d47eccf660cf6535c3febcbb3f88e54cb7a43124014b57359 pool/main/z/z3/libz3-4_4.13.3-1_amd64.deb
2f6a2aeacfc925eba8b00ac9139bc4bfccf8cacb09eb93de067074b26948eef9 pool/main/libz/libzstd/libzstd1_1.5.7+dfsg-1_amd64.deb
7794643cf4560de2a7e3b069e8ca8511ad2a568d652ea285f0fe051f79bc9e99 pool/main/l/linux-signed-amd64/linux-image-6.12.107+deb13-amd64_6.12.107-1_amd64.deb
dc68dd53c7356598d005866da0d19d7bd33af2a62971aced624cdf04492902b4 pool/main/l/linux/linux-libc-dev_6.12.107-1_all.deb
0788e4b0b72db1bcf74a19cb24c3511e7770ad0b341f0f30e361701c3072f53e pool/main/m/media-types/media-types_13.0.0_all.deb
3e610f29321cdcc61337c86e6b4031ff60f0c9915fe19e6a862ac06480620ff4 pool/main/m/mesa/mesa-libgallium_25.0.7-2+deb13u1_amd64.deb
caf2f7d296b7522efe74e40a74bce762a3ce84c33b53b5e54e1248ac2e0a13a5 pool/main/m/mesa/mesa-vulkan-drivers_25.0.7-2+deb13u1_amd64.deb
76bafa92947ebd37b0a6f9ec6570412041f56edebc1c66d323731216d318471e pool/main/n/netbase/netbase_6.5_all.deb
f155c8191ae6d41da73d792f4182680aeafeb85c3dd223934ee9fdd115c4f1fa pool/main/o/openssl/openssl-provider-legacy_3.5.7-1~deb13u2_amd64.deb
c1388174dc7140e6c19a6dd39f8a0dcb38a835cd66c65644e0dded43d5223cb0 pool/main/p/python3-defaults/python3_3.13.5-1_amd64.deb
a5d2371bebf3689f676e3f3bae800371bcc4392a8a51269e23d528249b399615 pool/main/p/python3-defaults/python3-minimal_3.13.5-1_amd64.deb
460c1fd72845516ac3ce9d1686916f3ab2e4c86f8a9746e14d26243a47e2b98d pool/main/p/python3.13/python3.13_3.13.5-2+deb13u5_amd64.deb
c1fda9e7b1bf15b2fa75a238d8a8710e142fe14c722fc650c5fc614f078c9073 pool/main/p/python3.13/python3.13-minimal_3.13.5-2+deb13u5_amd64.deb
f77e8aa0bf0de618f11cb0b21658a4ed60f177d63b3cf26d7fc3706b6d0eaaa9 pool/main/r/readline/readline-common_8.2-6_all.deb
32ac0692694f8a34cc90c895f4fc739680fb2ef0e2d4870a68833682bf1c81a3 pool/main/r/rpcsvc-proto/rpcsvc-proto_1.4.3-1_amd64.deb
214c02e1aa291076a3147b8a4c4cae02fadf4c67df629186e3e313726faef1de pool/main/t/tar/tar_1.35+dfsg-3.1_amd64.deb
813b9bbbe14ccda2b24617b8bade0ade4d745957e75921766c153d249886bd4c pool/main/t/tzdata/tzdata_2026c-0+deb13u1_all.deb
6a6764500eb8171d3cd6d793c09902974f8ea87bd20c68d9eac5f1eeaf3fc407 pool/main/x/xorgproto/x11proto-dev_2024.1-1_all.deb
168345058319094e475a87ace66f5fb6ae802109650ea8434d672117982b5d0a pool/main/x/xorg-sgml-doctools/xorg-sgml-doctools_1.11-1.1_all.deb
9ce1af9464faee0c679348dd11cdf63934c12e734a64e0903692b0cb5af38e06 pool/main/x/xtrans/xtrans-dev_1.4.0-1_all.deb
015be740d6236ad114582dea500c1d907f29e16d6db00566ca32fb68d71ac90d pool/main/z/zlib/zlib1g_1.3.dfsg+really1.3.1-1+b1_amd64.deb
"""
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


def fetch(cache, digest, pool_path, snapshot=SNAPSHOT):
    target = cache / Path(pool_path).name
    if target.exists():
        if sha256(target) != digest:
            raise RuntimeError(f"Cached file fails SHA-256: {target}")
        return target
    partial = target.with_name(target.name + f".{os.getpid()}.partial")
    urls = [f"https://deb.debian.org/debian/{pool_path}",
            f"https://snapshot.debian.org/archive/debian/{snapshot}/{pool_path}"]
    failures = []
    for url in urls:
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
    raise RuntimeError(f"Cannot download {pool_path}: {'; '.join(failures)}")


def deb_tar(path):
    """Read the data member of a Debian ar archive without invoking its code."""
    with path.open("rb") as stream:
        if stream.read(8) != b"!<arch>\n":
            raise ValueError(f"Not a Debian ar archive: {path}")
        while header := stream.read(60):
            if len(header) != 60 or header[58:] != b"`\n":
                raise ValueError("Invalid ar header")
            size = int(header[48:58])
            name = header[:16].decode().strip().rstrip("/")
            data = stream.read(size)
            if len(data) != size:
                raise ValueError("Truncated ar member")
            if size % 2:
                stream.read(1)
            if name.startswith("data.tar"):
                return tarfile.open(fileobj=io.BytesIO(data), mode="r:*")
    raise ValueError(f"No data archive: {path}")


def cpio_entry(out, inode, name, mode, data=b"", rdev=(0, 0)):
    encoded = name.encode() + b"\0"
    fields = [inode, mode, 0, 0, 1, 0, len(data), 0, 0, *rdev, len(encoded), 0]
    header = b"070701" + b"".join(f"{value:08x}".encode() for value in fields)
    out.write(header + encoded)
    out.write(b"\0" * (-(len(header) + len(encoded)) % 4))
    out.write(data)
    out.write(b"\0" * (-len(data) % 4))


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
        expected = json.loads(manifest.read_text())
        if any(sha256(build / name) != digest for name, digest in expected.items()):
            raise RuntimeError(f"Assembled guest cache fails SHA-256: {build}")
        return kernel, initrd
    files = {}
    modules = {}
    for package in packages:
        with deb_tar(package) as archive:
            for member in archive:
                name = str(PurePosixPath(member.name))
                if name == ".":
                    continue
                if name.startswith("/") or ".." in PurePosixPath(name).parts:
                    raise ValueError(f"Unsafe archive path: {name}")
                if name.startswith("boot/vmlinuz-"):
                    partial_kernel = kernel.with_suffix(f".{os.getpid()}.partial")
                    partial_kernel.write_bytes(archive.extractfile(member).read())
                    partial_kernel.replace(kernel)
                    continue
                if "/modules/" in name:
                    if re.search(r"\.ko(?:\.xz)?$", name):
                        key = Path(name).name.split(".ko")[0].replace("-", "_")
                        modules[key] = (name, archive.extractfile(member).read())
                    continue
                if name.startswith(("boot/", "usr/share/man/", "usr/share/locale/")):
                    continue
                if name.startswith("usr/share/doc/") and not name.endswith("/copyright"):
                    continue
                # No package privilege bits are needed by this one-shot root init.
                mode = member.mode & 0o777
                if member.isdir():
                    files[name] = (stat.S_IFDIR | mode, b"")
                elif member.issym():
                    files[name] = (stat.S_IFLNK | 0o777, member.linkname.encode())
                elif member.isfile() or member.islnk():
                    files[name] = (stat.S_IFREG | mode, archive.extractfile(member).read())
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
    init = "#!/bin/sh\nexec /usr/bin/python3 /probe/guest.py\n"
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


def repeatability(runs):
    """Compare scene hashes and complete capset bytes, not timing-dependent logs."""
    groups = {}
    blocked = set()
    for run in runs:
        if run.get("status") == "BLOCKED":
            blocked.add(run["device"])
        guest = run.get("guest", {})
        fingerprint = {"capset": guest.get("venus_capset"),
                       "scenes": {g["name"]: g.get("scene") for g in guest.get("gl", [])}}
        groups.setdefault(run["device"], []).append(fingerprint)
    return {device: {"runs": len(values), "status": "BLOCKED" if device in blocked else
                     "FAIL" if not all(v["scenes"] for v in values) else
                     "PASS" if len(values) > 1 and all(v == values[0] for v in values) else
                     "NOT_RUN" if len(values) == 1 else "FAIL"}
            for device, values in groups.items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__ or "Pinned Linux GPU reference for FerrionOS (no host root).")
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    parser.add_argument("--vk-driver", default=os.environ.get("VK_DRIVER_FILES", ""), help="Host Vulkan ICD JSON, or colon-separated ICD files")
    parser.add_argument("--render-node", default="/dev/dri/renderD128", help="Host EGL render node (recorded independently of the Vulkan ICD)")
    parser.add_argument("--out", type=Path, default=ROOT / "build/gpu-reference")
    parser.add_argument("--cache", type=Path, default=ROOT / "build/gpu-reference/cache" / SNAPSHOT)
    parser.add_argument("--repeat", type=int, default=2, help="Boots per device line; default 2 checks repeatability")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--fetch-only", action="store_true", help="Verify/download pinned inputs without booting")
    parser.add_argument("--variant", choices=("all", "debian13", "mesa26"), default="all",
                        help="Debian 13 baseline, newer Mesa runtime overlay, or both (default)")
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
    variants = [("debian13", {"mesa": MESA, "glibc": "2.41-12+deb13u4", "llvm": "1:19.1.7-3+b1",
                              "snapshot": SNAPSHOT, "kind": "Debian 13 baseline"}),
                ("mesa26", {"mesa": CURRENT_MESA, "glibc": CURRENT_GLIBC, "llvm": CURRENT_LLVM,
                            "snapshot": CURRENT_SNAPSHOT, "kind": "Debian 13 with minimal runtime overlay",
                            "index_sha256": CURRENT_INDEX_SHA256, "inrelease_sha256": CURRENT_RELEASE_SHA256})]
    record = {"schema_version": 2, "status": "FAIL", "started_utc": stamp,
              "base_pin": {"snapshot": SNAPSHOT, "index_sha256": INDEX_SHA256,
                           "inrelease_sha256": RELEASE_SHA256, "kernel": KERNEL},
              "expected_scene_hash": SCENE_HASH,
              "variants": [{"name": name, "pin": pin, "runs": [], "status": "NOT_RUN"}
                           for name, pin in variants if args.variant in ("all", name)]}
    try:
        record["host"] = host_info(args)
        if not args.fetch_only:
            preflight(args, record["host"])
        pins = [line.split() for line in PACKAGE_PINS.splitlines() if line.strip()]
        print(f"Checking {len(pins)} pinned Debian packages", flush=True)
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            packages = list(pool.map(lambda pair: fetch(args.cache, *pair), pins))
        record["base_inputs"] = [{"filename": p.name, "sha256": digest} for p, (digest, _) in zip(packages, pins)]
        for variant in record["variants"]:
            variant_name = variant["name"]
            variant["status"] = "FAIL"
            variant_packages = packages
            if variant_name == "mesa26":
                overlay_pins = [line.split() for line in CURRENT_PINS.splitlines() if line.strip()]
                overlay_cache = args.cache / CURRENT_SNAPSHOT
                overlay_cache.mkdir(exist_ok=True)
                print(f"Checking {len(overlay_pins)} pinned Mesa runtime overlay packages", flush=True)
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                    overlay = list(pool.map(lambda pair: fetch(overlay_cache, *pair, snapshot=CURRENT_SNAPSHOT), overlay_pins))
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
    except (OSError, ValueError, RuntimeError, tarfile.TarError) as error:
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
