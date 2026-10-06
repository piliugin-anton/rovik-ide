{ pkgs ? import <nixpkgs> {} }:
let
  version = "1.0";
in {
  packages.default = pkgs.hello;
  devShell = pkgs.mkShell { buildInputs = [ pkgs.git ]; };
}
