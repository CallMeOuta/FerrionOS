#!/usr/bin/env bash


FILE=${2:-recipe.toml}

RECIPE_PATH=$(make find.$1)
RECIPE_PATH=${RECIPE_PATH//$'\r'/}

cat "$RECIPE_PATH/$FILE"
