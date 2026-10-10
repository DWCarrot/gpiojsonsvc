# Updating the WSL kernel for `gpio-sim`

This guide updates the custom WSL2 kernel used for `gpio-sim` development. It
builds an exact Microsoft WSL kernel release with `CONFIG_GPIO_SIM=y`, installs
the versioned kernel image in the Windows user profile, and keeps the previous
image available for rollback.

The commands below use the versions and account from the initial setup as an
example:

- WSL distribution: `Debian`
- Windows user: `karrot`
- Kernel version: `6.18.40.1`

Change those example values when updating to a newer release.

Run steps 1 through 6 in the same Debian shell so their variables remain
defined. If a new shell is opened, rerun the variable assignments from the
earlier steps before continuing.

## When to update

Debian's `apt upgrade` does not update the WSL kernel. WSL itself and its
packaged kernel are updated from Windows:

```powershell
wsl --update
wsl --version
```

Run those commands periodically and when a WSL or kernel security update is
announced. Compare the `Kernel version` reported by `wsl --version` with the
custom kernel currently running:

```bash
uname -r
```

The custom kernel is selected by `%USERPROFILE%\.wslconfig`, so `wsl --update`
does not automatically replace it. Rebuild when the packaged kernel is newer
and the matching source tag is available. There is no need to rebuild merely
because Debian packages changed.

## 1. Record the new version and verify its source tag

Run the following inside Debian. Set `TARGET_VERSION` to the kernel version
shown by `wsl --version`. A WSL packaging suffix such as `-1` is not part of
the source tag; for example, packaged version `6.18.40.1-1` uses source tag
`linux-msft-wsl-6.18.40.1`.

```bash
TARGET_VERSION=6.18.40.1
KERNEL_TAG="linux-msft-wsl-${TARGET_VERSION}"
KERNEL_REPO=https://github.com/microsoft/WSL2-Linux-Kernel.git

git ls-remote --exit-code --tags "$KERNEL_REPO" "refs/tags/${KERNEL_TAG}"
```

Do not substitute a nearby branch or tag. Wait until the exact tag is
published if this command does not print a matching reference.

## 2. Install build prerequisites

The packages only need to be installed once, but running this again is safe:

```bash
sudo apt-get update
sudo apt-get install \
    bc bison build-essential cpio dwarves flex git libelf-dev libssl-dev \
    pkg-config rsync
```

The `gpiod` package supplies the command-line tools used by the smoke test. On
Debian 12, install the libgpiod 2.x version from bookworm-backports:

```bash
sudo apt-get install -t bookworm-backports gpiod
gpioinfo --version
```

The reported libgpiod version must be 2.x.

## 3. Clone the exact kernel source

Use a new directory for each release. The explicit Git setting prevents a
Windows-oriented global `core.autocrlf=true` setting from converting kernel
scripts to CRLF during checkout.

```bash
KERNEL_SRC="$HOME/src/WSL2-Linux-Kernel-${TARGET_VERSION}"

test ! -e "$KERNEL_SRC" || {
    echo "Refusing to overwrite existing path: $KERNEL_SRC" >&2
    exit 1
}

mkdir -p "$HOME/src"
git -c core.autocrlf=false clone \
    --depth 1 \
    --branch "$KERNEL_TAG" \
    "$KERNEL_REPO" \
    "$KERNEL_SRC"

git -C "$KERNEL_SRC" config core.autocrlf false
cd "$KERNEL_SRC"
git describe --tags --exact-match
```

The last command must print the value of `KERNEL_TAG`.

## 4. Enable `CONFIG_GPIO_SIM=y`

The Microsoft WSL configuration is `Microsoft/config-wsl`. First inspect the
GPIO-related options supplied by the selected tag:

```bash
cd "$KERNEL_SRC"
grep -E '^(# )?CONFIG_(GPIO_SIM|GPIOLIB|GPIO_CDEV|CONFIGFS_FS)=' \
    Microsoft/config-wsl
```

Before modification, `GPIO_SIM` will normally look like this:

```text
# CONFIG_GPIO_SIM is not set
```

That line means the feature is disabled. Enable it with the kernel's
non-interactive configuration helper:

```bash
./scripts/config \
    --file Microsoft/config-wsl \
    --enable GPIO_SIM

make KCONFIG_CONFIG=Microsoft/config-wsl olddefconfig
```

`scripts/config --enable GPIO_SIM` selects the built-in `y` state. The
`olddefconfig` step resolves dependencies and updates generated configuration
defaults for the selected source release.

Verify the result explicitly before building:

```bash
grep -qx 'CONFIG_GPIO_SIM=y' Microsoft/config-wsl
grep -qx 'CONFIG_GPIOLIB=y' Microsoft/config-wsl
grep -qx 'CONFIG_GPIO_CDEV=y' Microsoft/config-wsl
grep -qx 'CONFIG_CONFIGFS_FS=y' Microsoft/config-wsl

grep -E '^CONFIG_(GPIO_SIM|GPIOLIB|GPIO_CDEV|CONFIGFS_FS)=' \
    Microsoft/config-wsl
```

Expected output includes:

```text
CONFIG_GPIOLIB=y
CONFIG_GPIO_CDEV=y
CONFIG_GPIO_SIM=y
CONFIG_CONFIGFS_FS=y
```

The possible `GPIO_SIM` states are:

- `# CONFIG_GPIO_SIM is not set`: disabled.
- `CONFIG_GPIO_SIM=m`: build a loadable module. This also requires installing
  a matching WSL module VHD and is not the workflow described here.
- `CONFIG_GPIO_SIM=y`: build the simulator into the kernel image. This is the
  intended setting for this project.

Do not edit the generated `.config` in the source root. Every build command in
this guide explicitly uses `Microsoft/config-wsl` through `KCONFIG_CONFIG`.

## 5. Verify the release name and build

Confirm that the build will retain Microsoft's normal WSL release name. This
allows it to use the module set supplied with the same WSL kernel release.

```bash
EXPECTED_RELEASE="${TARGET_VERSION}-microsoft-standard-WSL2"
ACTUAL_RELEASE="$(make -s KCONFIG_CONFIG=Microsoft/config-wsl kernelrelease)"

printf 'expected: %s\nactual:   %s\n' "$EXPECTED_RELEASE" "$ACTUAL_RELEASE"
test "$ACTUAL_RELEASE" = "$EXPECTED_RELEASE"
```

Stop if the comparison fails. Do not install an image with an unexpected
release name.

Build the kernel. Twelve jobs worked comfortably on the original 16 GiB WSL
environment; reduce `BUILD_JOBS` on a smaller VM:

```bash
BUILD_JOBS=12
BUILD_LOG="$KERNEL_SRC/build.log"

set -o pipefail
make -j"$BUILD_JOBS" KCONFIG_CONFIG=Microsoft/config-wsl \
    2>&1 | tee "$BUILD_LOG"
```

The command must exit successfully. Verify the image and its embedded
configuration:

```bash
test -s arch/x86/boot/bzImage
ls -lh arch/x86/boot/bzImage
sha256sum arch/x86/boot/bzImage

./scripts/extract-ikconfig arch/x86/boot/bzImage |
    sed -n '/^CONFIG_LOCALVERSION=/p; /^CONFIG_GPIO_CDEV=/p; /^CONFIG_GPIO_SIM=/p; /^CONFIG_CONFIGFS_FS=/p'
```

The extracted configuration must contain `CONFIG_GPIO_SIM=y`.

This procedure changes only an option built into the kernel. If future work
changes options from disabled or built-in to `=m`, follow Microsoft's module
VHD instructions instead of reusing the packaged module set.

## 6. Install the versioned image

Set `WINDOWS_USER` to the Windows account that owns `.wslconfig`:

```bash
WINDOWS_USER=karrot
WINDOWS_PROFILE="/mnt/c/Users/${WINDOWS_USER}"
KERNEL_DEST_DIR="$WINDOWS_PROFILE/wsl-kernels"
KERNEL_DEST="$KERNEL_DEST_DIR/bzImage-gpio-sim-${TARGET_VERSION}"

install -d -m 0755 "$KERNEL_DEST_DIR"
install -m 0644 arch/x86/boot/bzImage "$KERNEL_DEST"

sha256sum arch/x86/boot/bzImage "$KERNEL_DEST"
```

Both hashes must match. Do not overwrite or remove the previous kernel image
yet; it is the rollback image.

## 7. Update `.wslconfig`

Save all work in WSL before continuing. In Windows PowerShell, back up the
current configuration:

```powershell
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
if (Test-Path "$env:USERPROFILE\.wslconfig") {
    Copy-Item "$env:USERPROFILE\.wslconfig" "$env:USERPROFILE\.wslconfig.$stamp.bak"
}

notepad "$env:USERPROFILE\.wslconfig"
```

Keep any unrelated settings. Under the `[wsl2]` section, replace only the
`kernel` value. For example:

```ini
[wsl2]
kernel=C:\\Users\\karrot\\wsl-kernels\\bzImage-gpio-sim-6.18.40.1
```

Use the new `TARGET_VERSION` in the filename. The path must be an absolute
Windows path; the doubled backslashes match Microsoft's documented format.

Apply the change from PowerShell:

```powershell
wsl --shutdown
```

This immediately stops every running WSL distribution, so do not run it until
editors, shells, and builds have been saved.

## 8. Verify after restart

Start Debian again and run:

```bash
TARGET_VERSION=6.18.40.1
EXPECTED_RELEASE="${TARGET_VERSION}-microsoft-standard-WSL2"

test "$(uname -r)" = "$EXPECTED_RELEASE"
zgrep -E '^CONFIG_(GPIO_SIM|GPIO_CDEV|CONFIGFS_FS)=' /proc/config.gz
test -d /sys/kernel/config/gpio-sim
gpioinfo --version
```

The release must equal `EXPECTED_RELEASE`, the three configuration options
must be `y`, and the configfs directory must exist.

Create, inspect, read, and remove a temporary simulated chip:

```bash
sudo bash <<'EOF'
set -euo pipefail

sim="/sys/kernel/config/gpio-sim/gpiojsonsvc-smoke-$$"

cleanup() {
    if [[ -d "$sim" ]]; then
        printf '0\n' > "$sim/live" 2>/dev/null || true
        rmdir "$sim/gpio-bank0/line0" 2>/dev/null || true
        rmdir "$sim/gpio-bank0" 2>/dev/null || true
        rmdir "$sim" 2>/dev/null || true
    fi
}
trap cleanup EXIT

mkdir "$sim"
mkdir "$sim/gpio-bank0"
printf '4\n' > "$sim/gpio-bank0/num_lines"
mkdir "$sim/gpio-bank0/line0"
printf 'smoke-line\n' > "$sim/gpio-bank0/line0/name"
printf '1\n' > "$sim/live"

chip="$(<"$sim/gpio-bank0/chip_name")"
test -c "/dev/$chip"

printf 'created_chip=%s\n' "$chip"
gpiodetect
gpioinfo -c "$chip"
value="$(gpioget --numeric -c "$chip" 0)"
printf 'line0_value=%s\n' "$value"
test "$value" = 0

printf 'gpio_sim_smoke_test=PASS\n'
EOF
```

The final line must be `gpio_sim_smoke_test=PASS`. The cleanup trap removes
the temporary chip whether the test passes or fails.

## Roll back a failed update

If Debian starts but the new kernel is unsuitable, edit `.wslconfig` in
PowerShell and restore the previous `kernel=` path, then restart WSL:

```powershell
notepad "$env:USERPROFILE\.wslconfig"
wsl --shutdown
```

If WSL cannot start at all, disable the entire custom configuration from
PowerShell:

```powershell
Rename-Item "$env:USERPROFILE\.wslconfig" ".wslconfig.gpio-sim.disabled"

wsl --shutdown
wsl -d Debian
```

Without the custom `kernel` setting, WSL falls back to Microsoft's packaged
kernel. The Debian filesystem is not removed or modified by this rollback.

## Clean up after a successful update

After the new kernel and smoke test have worked, the cloned source and build
artifacts are no longer required. Inspect the exact directory first:

```bash
TARGET_VERSION=6.18.40.1
KERNEL_SRC="$HOME/src/WSL2-Linux-Kernel-${TARGET_VERSION}"

printf '%s\n' "$KERNEL_SRC"
du -sh "$KERNEL_SRC"
```

Then remove that version's source tree if desired:

```bash
rm -rf -- "$KERNEL_SRC"
```

Keep the current kernel image and `.wslconfig`. It is also prudent to keep one
previous working kernel image and its `.wslconfig` backup until the next update
has been stable for a while.

## References

- [Microsoft WSL basic commands](https://learn.microsoft.com/windows/wsl/basic-commands)
- [Microsoft WSL advanced settings](https://learn.microsoft.com/windows/wsl/wsl-config)
- [Microsoft WSL2 kernel source and build instructions](https://github.com/microsoft/WSL2-Linux-Kernel)
- [Linux configfs GPIO simulator documentation](https://docs.kernel.org/admin-guide/gpio/gpio-sim.html)
