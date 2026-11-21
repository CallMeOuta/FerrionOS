#!/usr/bin/env bash



if [ -z "$*" ]
then
    echo "Find matching recipes, and format for inclusion in config"
    echo "Usage: $0 \"pattern\""
    echo "Must be run from the build system's root directory"
    echo "e.g. $0 \"TODO.*error\""
    exit 1
fi

cookbook_recipes="recipes"
recipe_paths=$(grep -rl "$*" "$cookbook_recipes" --include recipe.toml)

for recipe_path in $recipe_paths
do
    recipe_dir="$(dirname $recipe_path)"
    recipe_name="$(basename $recipe_dir)"
    echo "$recipe_name = {}    # " $(grep "$*" $recipe_path)
done