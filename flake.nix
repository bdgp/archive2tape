{
  inputs = {
    cargo2nix.url = "github:cargo2nix/cargo2nix/release-0.11.0";
    flake-utils.follows = "cargo2nix/flake-utils";
    nixpkgs.follows = "cargo2nix/nixpkgs";
  };

  outputs = inputs: with inputs;
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [cargo2nix.overlays.default];
        };

        rustPkgs = pkgs.rustBuilder.makePackageSet {
          rustVersion = "1.61.0";
          packageFun = import ./Cargo.nix;
        };

      in rec {
        packages = rec {
          ltfs = pkgs.stdenv.mkDerivation rec {
            pname =  "ltfs";
            version = "2.4.4.0-10470";
            src = builtins.fetchurl {
              url = "https://github.com/LinearTapeFileSystem/${pname}/archive/refs/tags/v${version}.tar.gz";
              sha256 = "14lpf1r8dyk08mdgb9xqi5y4m11bpdljyfzsfcj7bkg4a2xqj2c4";
            };
            buildInputs = with pkgs; [ autoconf automake m4 libtool pkg-config icu66 fuse libxml2 libuuid net-snmp ];
            preConfigure = ''
            ./autogen.sh
            '';
          };
          archive2tape = (rustPkgs.workspace.archive2tape {}).bin;
          archive2tape-image = let 
            rootfs = ./k8s/rootfs;
          in with pkgs; pkgs.dockerTools.streamLayeredImage {
            name = "fiji:443/bdgp/archive2tape";
            tag = "latest";
            contents = [
              bashInteractive coreutils findutils glibcLocales 
              archive2tape rotate-backups pigz gnugrep tini
              mtx mt-st fuse pspg less sqlite litecli
              archive2tape su-exec nss_ldap rootfs ltfs htop util-linux
              rsync gnutar gzip
            ];
            config = {
              Env = [
                "LOCALE_ARCHIVE=/lib/locale/locale-archive"
                "LD_LIBRARY_PATH=${nss_ldap}/lib:/lib"
              ];
            };
            extraCommands = ''
            mkdir -p -m 0755 usr/bin usr/local nix/store etc
            ln -sfvn /bin/env usr/bin/env
            '';
          };
          archive2tape-shell = pkgs.mkShell rec {
            LOCALE_ARCHIVE="${pkgs.glibcLocales}/lib/locale/locale-archive";
            buildInputs = with pkgs; [
              rustup gcc bashInteractive coreutils findutils glibcLocales 
              rotate-backups pigz gnugrep tini
              mtx mt-st fuse pspg less sqlite litecli
              ltfs util-linux rsync gnutar gzip
            ];
            shellHook = '' 
            export RUSTUP_HOME=$PWD/.rustup
            export PATH=$RUSTUP_HOME/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH
            '';
          };
        };
        default = packages.archive2tape-image;
        devShell = packages.archive2tape-shell;
      }
    );
}

