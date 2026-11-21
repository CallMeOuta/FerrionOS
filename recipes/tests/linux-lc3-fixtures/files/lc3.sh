# The LC3 script of plans/linux-compat.md for Debian's dash: the dynamic hello, the four-thread
# program, a posix_spawn of a missing file, and 30 core tools with find, grep, tar and gzip
# (plans/linux-compat/kit/cu.sh made deterministic: no dates, sizes of file systems, host names
# or listings that depend on the file system). It also checks what a session must not offer:
# a display, the X11 socket directory, a 32-bit dynamic linker, a writable /usr.
set -e
f=$(dirname "$0")
"$f/hello-dyn"; "$f/threads"; "$f/spawn"
[ -z "${DISPLAY:-}" ] && echo "no DISPLAY"
[ ! -e /tmp/.X11-unix ] && echo "no X11 socket directory"
[ ! -e /lib/ld-linux.so.2 ] && echo "no 32-bit dynamic linker"
if touch /usr/lc3 2>/dev/null; then echo "/usr is writable"; else echo "/usr refused a write"; fi
umask 022
d=$(mktemp -d); cd "$d"
ls /usr/lib/x86_64-linux-gnu > listing; wc -l < listing; sort -r listing | head -3 | cut -c1-12
mkdir -p a/b/c; touch a/b/c/file; ln -s a/b/c/file link; readlink link; stat -c '%s %a %U' listing
cp -a a a2; mv a2 a3; chmod 600 listing; ls; basename "$d/a/b/c" ; dirname a/b/c/file
sha256sum listing | cut -c1-16; md5sum < listing | cut -c1-8; cksum < listing | cut -d' ' -f2
id -un; id -u >/dev/null; uname -sr | cut -d' ' -f1; true; ! false
seq 1 1000 | tr '\n' ' ' | wc -c; printf 'b\na\nb\n' | sort | uniq | tee out.txt
echo hello | rev 2>/dev/null || echo hello | od -c | head -1
printf 'x y z\n' | cut -d' ' -f2; head -c 5 listing | wc -c; tail -n 1 listing
expr 6 \* 7; echo abc | tr a-c A-C; nl out.txt; paste out.txt out.txt
yes | head -n 2; printf '%s\n' 3 1 2 | sort -n | tac; seq 3 | numfmt --to=iec
echo data > g; cat g g | wc -l; split -l 1 out.txt part; ls part*; comm -12 out.txt out.txt
find . -type f | sort | wc -l; find . -name file | sort; grep -c 'lib' listing; grep -E '^a$' out.txt
tar czf t.tgz a; mkdir x; tar xzf t.tgz -C x; find x | sort; gzip -c g > g.gz; gunzip -c g.gz
TZ=UTC0 touch -d '2001-02-03 04:05:06' g; stat -c %Y g; rm -rf a a3; ls | wc -l; env | grep -c '^PATH='
cd /; rm -rf "$d"; echo "coreutils script done"
