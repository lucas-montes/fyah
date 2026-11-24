savedcmd_fyah_km.mod := printf '%s\n'   fyah_km.o | awk '!x[$$0]++ { print("./"$$0) }' > fyah_km.mod
