{
  # Android SDK composition: keep in sync with ~/programming/android/nix/versions.nix.
  platform = "36";
  buildTools = [
    "35.0.0"
    "34.0.0"
    "36.0.0"
  ];
  commandLineTools = "19.0";
  platformTools = "36.0.2";
  ndk = "28.2.13676358";
  cmake = "3.22.1";
  emulator = "36.5.11";
  rust = "1.95.0";

  # Minimum Android API for native code and the manifest.
  minSdk = 28;
}
