{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME" }:

let
  rustPlatform = pkgs.rustPlatform;

  godon-observer = rustPlatform.buildRustPackage {
    pname = "godon-observer";
    version = version;

    src = ./.;

    cargoHash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    buildInputs = with pkgs; [ openssl ];

    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    doCheck = false;

    buildPhase = ''
      echo "Building godon-observer..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-observer $out/bin/godon-observer
      chmod +x $out/bin/godon-observer
      echo "Installation completed"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    contents = [ godon-observer pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-observer}/bin/godon-observer" ];
      ExposedPorts = { "8089/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-observer}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "RUST_LOG=info"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--port=8089" ];
    };
  };

in { inherit godon-observer containerImage; }
