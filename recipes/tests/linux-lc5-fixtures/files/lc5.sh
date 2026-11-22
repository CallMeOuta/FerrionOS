f=$(dirname "$0")
curl -sS -o /tmp/page https://example.com/ 2>/tmp/curl.err
python3 "$f/lc5.py" "$1" "$?"
