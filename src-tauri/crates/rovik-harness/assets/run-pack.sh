#!/bin/sh
set -eu
exec /usr/bin/python3 /oracle/native_pack.py < "/oracle/requests/$1.json"
