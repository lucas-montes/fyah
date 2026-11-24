# SPDX-License-Identifier: GPL-2.0
#
# Build the fyah companion module against a prepared kernel tree.
#
#   make KDIR=/path/to/kernel/build
#
# The Nix package passes KDIR, the exact Rust toolchain the kernel was built
# with, and the kernel's bindgen. Everything else comes from Kbuild.

KDIR ?= /lib/modules/$(shell uname -r)/build

default:
	$(MAKE) -C $(KDIR) M=$$PWD

clean:
	$(MAKE) -C $(KDIR) M=$$PWD clean

.PHONY: default clean
