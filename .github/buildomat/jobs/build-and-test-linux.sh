#!/bin/bash
#:
#: name = "build-and-test (ubuntu-22.04)"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: rust_toolchain = "1.77.2"
#: output_rules = [
#:	"%/work/*",
#:	"%/var/tmp/omicron_tmp/*",
#:	"!/var/tmp/omicron_tmp/crdb-base*",
#:	"!/var/tmp/omicron_tmp/rustc*",
#: ]

exec .github/buildomat/build-and-test.sh linux
