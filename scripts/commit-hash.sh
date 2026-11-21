#!/usr/bin/env bash


set -e

if [ ! -d "recipes/core" ]
then
    echo "Error: recipes/core directory not found"
    exit 1
fi

for recipe_dir in recipes/core/*/
do
    recipe_name=$(basename "$recipe_dir")
    source_dir="$recipe_dir/source"
    
    if [ -d "$source_dir" ] && [ -d "$source_dir/.git" ]
    then
        commit_hash=$(cd "$source_dir" && git rev-parse HEAD)
        echo "$recipe_name: $commit_hash"
    fi
done
