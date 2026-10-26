# Sourced by both Linux AppImage packagers. These libraries either come from
# the distribution or are staged from the pinned SDK after linuxdeploy runs.
# Never copy a newer build host's implementation into the portable package.
LINUX_LIBRARY_OPTIONS=()
for library in 'libstdc++*' 'libgcc_s*' 'libfontconfig*' 'libfreetype*' \
               'libasound*' 'libXau*' 'libXdmcp*' 'libxcb*' 'libxkbcommon*' \
               'libbsd*' 'libmd*'; do
    LINUX_LIBRARY_OPTIONS+=(--exclude-library "$library")
done
