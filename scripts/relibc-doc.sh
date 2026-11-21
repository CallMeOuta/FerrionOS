#!/usr/bin/env bash


rm -rf build/relibc-doc build/relibc-doc.tar.gz
mkdir -p build/relibc-doc
make ri.relibc-doc DESTDIR=./build/relibc-doc CI=1
tar -czvf ./build/relibc-doc.tar.gz ./build/relibc-doc/usr/share/doc/relibc/
