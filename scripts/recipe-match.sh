#!/usr/bin/env bash


bat --decorations=always $(rg "$1" -li --sort=path recipes)
