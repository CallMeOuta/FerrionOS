#!/usr/bin/env bash



CATEGORY=${1:-wip/libs}

target/release/repo cook-list --all --display=csv |  grep ",$" |  grep ",recipes/$CATEGORY" | cut -d ',' -f2
