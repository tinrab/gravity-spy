{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config ];
  buildInputs = with pkgs; [
    alsa-lib vulkan-loader libxkbcommon
    libx11 libxcursor libxi libxrandr
  ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [
    alsa-lib vulkan-loader libxkbcommon
    libx11 libxcursor libxi libxrandr
  ]) + ":/run/opengl-driver/lib";
}
