{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME"
, root ? builtins.getEnv "PROJECT_ROOT" }:

let

  rustPlatform = pkgs.rustPlatform;

  godon-seeder = rustPlatform.buildRustPackage {
    pname = "godon-seeder";
    version = version;

    src = ./.;

    cargoHash = "sha256-FUt51XAlCbXzPOvq4yV8/osOZmiCIi+6g4V+XEa6LJY=";

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    buildInputs = with pkgs; [ openssl ];

    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    doCheck = false;

    buildPhase = ''
      echo "Building Rust godon-seeder..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-seeder $out/bin/godon-seeder
      chmod +x $out/bin/godon-seeder

      if [ -f "godon_seeder.sh" ]; then
        cp godon_seeder.sh $out/bin/
        chmod +x $out/bin/godon_seeder.sh
      fi

      echo "Installation completed successfully!"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    contents = [
      godon-seeder
      pkgs.cacert
      pkgs.busybox
      pkgs.git
      pkgs.bash
      (pkgs.writeTextDir "etc/passwd" ''
        root:x:0:0:root:/root:/bin/sh
        godon:x:1000:1000:godon:/var/lib/godon:/bin/sh
      '')
      (pkgs.writeTextDir "etc/group" ''
        root:x:0:
        godon:x:1000:
      '')
    ];

    config = {
      Entrypoint = [ "${godon-seeder}/bin/godon_seeder.sh" ];
      ExposedPorts = { "8080/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-seeder}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "WINDMILL_BASE_URL=http://windmill-app:8000/api"
        "WINDMILL_WORKSPACE=godon"
        "GODON_DIR=/var/lib/godon"
      ];
      WorkingDir = "/var/lib/godon";
      User = "1000:1000";
      Cmd = [ "--help" ];
    };

    fakeRootCommands = ''
      mkdir -p var/lib/godon
      chown -R 1000:1000 var/lib/godon
    '';
  };

in { inherit godon-seeder containerImage; }
