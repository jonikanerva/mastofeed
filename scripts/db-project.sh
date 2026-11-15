# shellcheck shell=bash
# The mastofeed settings of scripts/db.sh (STACK.md §3). scripts/db.sh is
# the same file in caltodo, mastofeed, and spot-price; the differences of
# this project are here.

DB_PROJECT=mastofeed
DB_ENV=MASTOFEED
DB_DEV_DATABASE=mastofeed
DB_TEST_DATABASE=mastofeed_dev
# Host ports 30000-39999. caltodo uses 20000-29999 and spot-price
# 40000-48999. All stay below the macOS ephemeral range (49152-65535).
DB_PORT_MIN=30000
DB_PORT_SPAN=10000
# Production requires TLS (STACK.md §5), and the tests of the migrate
# command connect with sslmode prefer, require, and disable.
DB_TLS=1
DB_PROJECT_COMMANDS=""
