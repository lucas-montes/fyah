savedcmd_fyah_km.o := OBJTREE=/nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build RUST_MODFILE=./fyah_km /nix/store/c6q7rin9gr1d7vnh7imsma1nacgh641f-rustc-1.95.0/bin/rustc --edition=2021 -Zbinary_dep_depinfo=y -Astable_features -Aunused_features -Dnon_ascii_idents -Dunsafe_op_in_unsafe_fn -Wmissing_docs -Wrust_2018_idioms -Wunreachable_pub -Wclippy::all -Wclippy::as_ptr_cast_mut -Wclippy::as_underscore -Wclippy::cast_lossless -Aclippy::collapsible_if -Aclippy::collapsible_match -Wclippy::ignored_unit_patterns -Aclippy::incompatible_msrv -Wclippy::mut_mut -Wclippy::needless_bitwise_bool -Aclippy::needless_lifetimes -Wclippy::no_mangle_with_rust_abi -Wclippy::ptr_as_ptr -Wclippy::ptr_cast_constness -Wclippy::ref_as_ptr -Wclippy::undocumented_unsafe_blocks -Aclippy::uninlined_format_args -Wclippy::unnecessary_safety_comment -Wclippy::unnecessary_safety_doc -Aclippy::unwrap_or_default -Wrustdoc::missing_crate_level_docs -Wrustdoc::unescaped_backticks -Cpanic=abort -Cembed-bitcode=n -Clto=n -Cforce-unwind-tables=n -Ccodegen-units=1 -Csymbol-mangling-version=v0 -Crelocation-model=static -Zfunction-sections=n -Wclippy::float_arithmetic --target=/nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/scripts/target.json -Ctarget-feature=-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2 -Zcf-protection=branch -Cjump-tables=n -Ctarget-cpu=x86-64 -Ztune-cpu=generic -Cno-redzone=y -Ccode-model=kernel -Zfunction-return=thunk-extern -Zpatchable-function-entry=16,16 -Copt-level=2 -Cdebug-assertions=n -Coverflow-checks=y -Cdebuginfo=2 --remap-path-prefix /nix/store/arg9jmc5491gzs3m9g0d7rwwxxm01k7v-rust-lib-src=/  --cfg MODULE  @/nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/include/generated/rustc_cfg -Zallow-features=arbitrary_self_types,asm_goto,generic_arg_infer,used_with_arg -Zcrate-attr=no_std -Zcrate-attr='feature(arbitrary_self_types,asm_goto,generic_arg_infer,used_with_arg)' -Zunstable-options --extern pin_init --extern kernel --extern zerocopy --extern zerocopy_derive --crate-type rlib -L /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/ --sysroot=/dev/null --out-dir ./ --emit=dep-info=./.fyah_km.o.d --emit=obj=fyah_km.o fyah_km.rs   ; /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/tools/objtool/objtool --hacks=jump_label --hacks=noinstr --hacks=skylake --ibt --prefix=16 --orc --retpoline --rethunk --sls --static-call --uaccess  --link  --module fyah_km.o

source_fyah_km.o := fyah_km.rs

deps_fyah_km.o := \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libcore.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libkernel.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libffi.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libcompiler_builtins.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libpin_init.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libpin_init_internal.so \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libmacros.so \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libbuild_error.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libbindings.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libzerocopy.rmeta \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libzerocopy_derive.so \
  /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/rust/libuapi.rmeta \

fyah_km.o: $(deps_fyah_km.o)

$(deps_fyah_km.o):

fyah_km.o: $(wildcard /nix/store/zfcszlxyla45fjhcmqmga7a87mf1hqk0-linux-7.2.15-dev/lib/modules/7.2.15/build/tools/objtool/objtool)
