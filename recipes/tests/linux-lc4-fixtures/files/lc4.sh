set -e
f=$(dirname "$0")
python3 -c 'import os, sys; print("one-liner", sys.version_info[:2], os.getuid(), sys.prefix)'
python3 "$f/lc4.py"
python3 -m venv /tmp/venv
/tmp/venv/bin/pip install --no-index --no-cache-dir --disable-pip-version-check "$f/six-1.17.0-py2.py3-none-any.whl"
/tmp/venv/bin/python -c 'import six, sys; print("six", six.__version__, sys.prefix, six.__file__)'
echo "python script done"
