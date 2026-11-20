# penny-servo

**This is a fork of [Servo](https://github.com/servo/servo)** for **Penny**, the web browser of
[CuBit OS](https://github.com/docandrew/cubit). It is not the Servo project, and it is not affiliated
with or endorsed by the Servo project, the Linux Foundation, or Mozilla.

- **Forked from:** `servo/servo` at commit `8319b662d03f` ("script: Use unrooted for
  is_instance_validatable and check_ancestors_disabled_state_for_form_control (#48396)").
- **Branch:** `penny`. Its first commit is an exact snapshot of that upstream commit: its tree hash,
  `9bcb65249ed44b677f61564d1a040bfeb7c8b334`, equals upstream's (`git rev-parse 8319b662d03f^{tree}` in
  a servo/servo clone). Every CuBit change is an ordinary commit after it; `git log --reverse penny`
  lists them. Highlights: the `cubitshell` embedder (Penny itself,
  in `ports/cubitshell`), HarfRust shaping in place of HarfBuzz (based on upstream PR #38707), a SPARK
  cryptography provider for rustls in place of aws-lc, connection and media-player budgets, and fixes
  for running on CuBit.
- **Direction:** this fork will diverge substantially from upstream. Non-Rust components (SpiderMonkey,
  media codecs, font parsing) are being replaced piece by piece with proved SPARK code. Upstream changes
  are not merged routinely.

## Licensing

- Servo's own files, including every upstream file we modified, remain under the **Mozilla Public
  License 2.0** (`LICENSE`), as does new code we add inside Servo's own components (so it stays
  upstreamable).
- New files written for Penny's shell in `ports/cubitshell` that are not derived from Servo carry
  `SPDX-License-Identifier: GPL-3.0-or-later` and are under the **GNU General Public License, version 3
  or later** (`LICENSE.GPL-3.0`). Files there derived from Servo's `servoshell` keep the MPL header.
- Each file's header states its license. MPL-2.0 §3.3 allows the combined program to be distributed
  under the GPL, as CuBit does; no file in this repository is marked "Incompatible With Secondary
  Licenses".
- Third-party crates keep their own licenses (see `Cargo.lock` and each crate's metadata).
- `LICENSE_WHATWG_SPECS` is unchanged from upstream.

---

*The upstream Servo README follows.*

# The Servo Parallel Browser Engine Project

Servo is a prototype web browser engine written in the
[Rust](https://github.com/rust-lang/rust) language. It is currently developed on
64-bit macOS, 64-bit Linux, 64-bit Windows, 64-bit OpenHarmony, and Android.

Servo welcomes contribution from everyone. Check out:

- The [Servo Book](https://book.servo.org) for documentation
- [servo.org](https://servo.org/) for news and guides

Coordination of Servo development happens:
- Here in the Github Issues
- On the [Servo Zulip](https://servo.zulipchat.com/)
- In video calls advertised in the [Servo Project](https://github.com/servo/project/issues) repo.

## Getting started

For more detailed build instructions, see the Servo Book under [Getting the Code] and [Building Servo].

[Getting the Code]: https://book.servo.org/building/getting-the-code.html
[Building Servo]: https://book.servo.org/building/building.html

### macOS

- Download and install [Xcode](https://developer.apple.com/xcode/) and [`brew`](https://brew.sh/).
- Install `uv`: `curl -LsSf https://astral.sh/uv/install.sh | sh` 
- Install `rustup`: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Restart your shell to make sure `cargo` is available
- Install the other dependencies: `./mach bootstrap`
- Build servoshell: `./mach build`

### Linux

- Install `curl`:
  - Arch: `sudo pacman -S --needed curl`
  - Debian, Ubuntu: `sudo apt install curl`
  - Fedora: `sudo dnf install curl`
  - Gentoo: `sudo emerge net-misc/curl`
- Install `uv`: `curl -LsSf https://astral.sh/uv/install.sh | sh` 
- Install `rustup`: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Restart your shell to make sure `cargo` is available
- Install the other dependencies: `./mach bootstrap`
- Build servoshell: `./mach build`

### Windows

- Download [`uv`](https://docs.astral.sh/uv/getting-started/installation/#standalone-installer), and [`rustup`](https://win.rustup.rs/)
  - Be sure to select *Quick install via the Visual Studio Community installer*
- Ensure that [`winget`](https://learn.microsoft.com/en-us/windows/package-manager/winget/) is available. It should be preinstalled on Windows 10 1809+ and Windows 11, otherwise can be [`manually installed`](https://github.com/microsoft/winget-cli#installing-the-client).
- In the Visual Studio Installer, ensure the following components are installed:
  - **Windows 10/11 SDK (anything >= 10.0.19041.0)** (`Microsoft.VisualStudio.Component.Windows{10, 11}SDK.{>=19041}`)
  - **MSVC v143 - VS 2022 C++ x64/x86 build tools (Latest)** (`Microsoft.VisualStudio.Component.VC.Tools.x86.x64`)
  - **C++ ATL for latest v143 build tools (x86 & x64)** (`Microsoft.VisualStudio.Component.VC.ATL`)
- Restart your shell to make sure `cargo` is available
- Install the other dependencies: `.\mach bootstrap`
- Build servoshell: `.\mach build`

### Android

- Ensure that the following environment variables are set:
  - `ANDROID_SDK_ROOT`
  - `ANDROID_NDK_ROOT`: `$ANDROID_SDK_ROOT/ndk/28.2.13676358/`
 `ANDROID_SDK_ROOT` can be any directory (such as `~/android-sdk`).
  All of the Android build dependencies will be installed there.
- Install the latest version of the [Android command-line
  tools](https://developer.android.com/studio#command-tools) to
  `$ANDROID_SDK_ROOT/cmdline-tools/latest`.
- Run the following command to install the necessary components:
  ```shell
  sudo $ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager --install \
   "build-tools;36.0.0" \
   "emulator" \
   "ndk;29.0.14206865" \
   "platform-tools" \
   "platforms;android-37" \
   "system-images;android-37;google_apis;x86_64"
  ```
- Follow the instructions above for the platform you are building on

### OpenHarmony

- Follow the instructions above for the platform you are building on to prepare the environment.
- Depending on the target distribution (e.g. `HarmonyOS NEXT` vs pure `OpenHarmony`) the build configuration will differ slightly.
- Ensure that the following environment variables are set
  - `DEVECO_SDK_HOME` (Required when targeting `HarmonyOS NEXT`)
  - `OHOS_BASE_SDK_HOME` (Required when targeting `OpenHarmony`)
  - `OHOS_SDK_NATIVE` (e.g. `${DEVECO_SDK_HOME}/default/openharmony/native` or `${OHOS_BASE_SDK_HOME}/${API_VERSION}/native`)
  - `SERVO_OHOS_SIGNING_CONFIG`: Path to json file containing a valid signing configuration for the demo app.
- Review the detailed instructions at [Building for OpenHarmony].
- The target distribution can be modified by passing `--flavor=<default|harmonyos>` to `mach <build|package|install>`.
