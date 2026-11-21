set -e
d=$(mktemp -d); cd "$d"
i=0; while [ $i -lt 5 ]; do echo "line$i" >> f; i=$((i+1)); done
cat f | grep line | sort -r | wc -l
sort -r f | head -n 2 | tr a-z A-Z
sed 's/line/L/' f > g; cat g
ls; cp f h; mv h k; ls; rm k
mkdir sub; touch sub/a sub/b; ls sub | wc -l; rm -r sub; ls
wc -c < f
date +%Y >/dev/null && echo "date ok"
[ "$(id -u)" = "$EXPECT_UID" ] && echo "uid ok"
uname -s; uname -m
sleep 0.1
(echo sub; exit 3) || echo "rc=$?"
x=$(echo captured | tr a-z A-Z); echo "$x"
cat <<EOF
here $i $x
EOF
trap 'echo "got USR1"' USR1
kill -USR1 $$
sleep 0.2 &
p=$!
wait $p; echo "bg rc=$?"
sleep 5 & q=$!
kill $q; wait $q || echo "killed rc=$?"
trap - USR1
cd /; rm -r "$d"; echo "busybox script done"
