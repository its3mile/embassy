#!/bin/sh
# set -eu
#
# This script executes before the dev container is created from the build image
#

RPI_PICO_HOST_PATH=$(find / -type d -name 'RPI-*' 2>/dev/null | head -1)
if ![ -n $RPI_PICO_HOST_PATH ]; then
    RPI_PICO_HOST_PATH=/dev/null
fi
echo "RPI_PICO_HOST_PATH=$RPI_PICO_HOST_PATH" > .devcontainer/.env
