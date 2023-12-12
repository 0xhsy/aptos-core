#!/bin/bash

# Copyright © Aptos Foundation
# SPDX-License-Identifier: Apache-2.0

# A light wrapper for the new forge python script

export RUST_LOG_FORMAT=text

echo "Warning: run_forge.sh is deprecated. Please use forge.py instead."
echo "Executing python testsuite/forge.py test $@"
exec python3 testsuite/forge.py test "$@"
