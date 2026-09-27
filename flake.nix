{
  description = "ytmdl: YouTube music downloader (Rust + embedded yt-dlp) for arm64 Android";

  inputs = {
    # Same pins as ~/programming/android so the SDK/NDK/Rust derivations are shared.
    nixpkgs.url = "github:NixOS/nixpkgs/93108a538f079596c9a16c72cf03e9322782b6dd";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
        config = {
          allowUnfree = true;
          android_sdk.accept_license = true;
        };
      };
      inherit (pkgs) lib;
      versions = import ./nix/versions.nix;

      # Must stay identical to ~/programming/android's composition to reuse its store paths.
      android = pkgs.androidenv.composeAndroidPackages {
        cmdLineToolsVersion = versions.commandLineTools;
        platformToolsVersion = versions.platformTools;
        platformVersions = [ versions.platform ];
        buildToolsVersions = versions.buildTools;
        includeNDK = true;
        ndkVersions = [ versions.ndk ];
        includeCmake = true;
        cmakeVersions = [ versions.cmake ];
        includeEmulator = true;
        emulatorVersion = versions.emulator;
        includeSystemImages = true;
        systemImageTypes = [ "google_apis" ];
        abiVersions = [ "x86_64" ];
      };
      sdk = "${android.androidsdk}/libexec/android-sdk";
      ndk = "${sdk}/ndk/${versions.ndk}";
      ndkBin = "${ndk}/toolchains/llvm/prebuilt/linux-x86_64/bin";
      api = toString versions.minSdk;

      rust = pkgs.rust-bin.stable.${versions.rust}.default.override {
        extensions = [
          "rust-src"
          "rust-analyzer"
        ];
        targets = [
          "aarch64-linux-android"
          "x86_64-linux-android"
        ];
      };

      # Host interpreter for desktop builds; same minor version as python.org's Android build.
      python = pkgs.python314;

      env = {
        JAVA_HOME = pkgs.jdk17.home;
        ANDROID_HOME = sdk;
        ANDROID_SDK_ROOT = sdk;
        ANDROID_NDK_HOME = ndk;
        ANDROID_NDK_ROOT = ndk;
        NDK_HOME = ndk;
        ANDROID_NDK_VERSION = versions.ndk;
        YTMDL_ANDROID_API = api;
        GRADLE_OPTS = "-Dorg.gradle.project.android.aapt2FromMavenOverride=${sdk}/build-tools/${builtins.head versions.buildTools}/aapt2";
        # 16 GB desktop shared with an emulated Android guest.
        CARGO_BUILD_JOBS = "2";
        CMAKE_BUILD_PARALLEL_LEVEL = "2";
        LIBCLANG_PATH = "${lib.getLib pkgs.llvmPackages.libclang}/lib";
        CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = "${ndkBin}/aarch64-linux-android${api}-clang";
        CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER = "${ndkBin}/x86_64-linux-android${api}-clang";
        CC_aarch64_linux_android = "${ndkBin}/aarch64-linux-android${api}-clang";
        CC_x86_64_linux_android = "${ndkBin}/x86_64-linux-android${api}-clang";
        AR_aarch64_linux_android = "${ndkBin}/llvm-ar";
        AR_x86_64_linux_android = "${ndkBin}/llvm-ar";
        # Desktop PyO3 builds and the embedded desktop runtime.
        PYO3_PYTHON = "${python}/bin/python3.14";
        YTMDL_PYTHON_HOME = "${python}";
        YTMDL_QJS = "${lib.getBin pkgs.quickjs-ng}/bin/qjs";
      };

      gradleProperties = pkgs.writeText "ytmdl-gradle.properties" ''
        org.gradle.daemon=false
        org.gradle.parallel=false
        org.gradle.workers.max=2
        org.gradle.jvmargs=-Xmx1536m -XX:MaxMetaspaceSize=768m -Dfile.encoding=UTF-8
        kotlin.compiler.execution.strategy=in-process
        android.aapt2FromMavenOverride=${sdk}/build-tools/${builtins.head versions.buildTools}/aapt2
      '';

      tools = [
        android.androidsdk
        pkgs.jdk17
        rust
        pkgs.dioxus-cli
        pkgs.cargo-ndk
        python
        pkgs.quickjs-ng
        pkgs.pkg-config
        pkgs.openssl
        pkgs.cmake
        pkgs.ninja
        pkgs.git
        pkgs.curl
        pkgs.wget
        pkgs.unzip
        pkgs.file
        pkgs.jq
        pkgs.ffmpeg-headless # ffprobe, for checking downloaded files only
        pkgs.cargo-about # tools/gen-notices
        # arm64 test layers
        pkgs.qemu-user
        pkgs.erofs-utils
        pkgs.e2fsprogs
      ];

      # Shared by the dev shell and the apps; sets $root to the checkout.
      setup = ''
        export ANDROID_USER_HOME="''${XDG_DATA_HOME:-$HOME/.local/share}/android-nix"
        export GRADLE_USER_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}/android-nix/gradle"
        mkdir -p "$GRADLE_USER_HOME"
        ln -sfn ${gradleProperties} "$GRADLE_USER_HOME/gradle.properties"
        export PATH="${sdk}/cmake/${versions.cmake}/bin:${ndkBin}:$PATH"
        export RUST_SRC_PATH="${rust}/lib/rustlib/src/rust/library"
        # Pieces downloaded by tools/fetch-deps.
        root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
        export YTMDL_YTDLP="$root/.deps/yt-dlp/yt-dlp.zip"
      '';

      # `nix run .#<name>` in the checkout: `text` runs there with the dev
      # shell's tools and environment.
      app = name: description: text: {
        type = "app";
        meta.description = description;
        program = lib.getExe (
          pkgs.writeShellApplication {
            name = "ytmdl-${name}";
            runtimeInputs = tools;
            text =
              lib.concatStrings (lib.mapAttrsToList (k: v: "export ${k}=${lib.escapeShellArg v}\n") env)
              + setup
              + ''
                if [ ! -x "$root/tools/build-apk" ]; then
                  echo "Run this inside the ytmdl checkout." >&2
                  exit 1
                fi
                cd "$root"
              ''
              + text;
          }
        );
      };
    in
    {
      devShells.${system}.default = pkgs.mkShell (
        env
        // {
          packages = tools;
          RA_SERVER_PATH = "${rust}/bin/rust-analyzer";
          shellHook = setup;
        }
      );

      apps.${system} = {
        # nix run .#build-apk [-- --debug | --no-autotest]
        build-apk = app "build-apk" "Build target/ytmdl-arm64.apk" ''
          exec tools/build-apk "$@"
        '';

        # nix run .#install [-- [--build] [build-apk options]]
        install =
          app "install"
            "Install the APK on the phone over adb, building it first if the source changed (--build: always)"
            ''
              force=false
              if [ "''${1:-}" = --build ]; then
                force=true
                shift
              fi
              # Before building, so a missing phone doesn't cost a build.
              if ! state="$(adb get-state 2>&1)"; then
                echo "adb: $state" >&2
                echo "Connect the phone with USB debugging on (with several devices, set ANDROID_SERIAL)." >&2
                exit 1
              fi
              apk=target/ytmdl-arm64.apk
              if $force; then
                tools/build-apk "$@"
              elif [ ! -f "$apk" ]; then
                echo "No APK yet; building it."
                tools/build-apk "$@"
              elif [ "$(tools/source-stamp)" != "$(cat "$apk.stamp" 2>/dev/null)" ]; then
                echo "The source changed since the APK was built; building it again."
                tools/build-apk "$@"
              else
                echo "The APK is up to date with the source."
              fi
              # -r keeps the app's data: library, playlists, settings.
              adb install -r "$apk"
            '';
      };

      formatter.${system} = pkgs.nixfmt;
    };
}
