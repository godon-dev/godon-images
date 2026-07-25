{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME" }:

let
  rustPlatform = pkgs.rustPlatform;

  godon-bench-generic = rustPlatform.buildRustPackage {
    pname = "godon-bench-generic";
    version = version;

    src = ./.;

    cargoLock.lockFile = ./Cargo.lock;

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    buildInputs = with pkgs; [ openssl ];

    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    doCheck = true;

    checkPhase = ''
      echo "Running unit tests..."
      export HOME=$TMPDIR
      cargo test 2>&1
    '';

    buildPhase = ''
      echo "Building godon-bench-generic..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-bench-generic $out/bin/godon-bench-generic
      chmod +x $out/bin/godon-bench-generic
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    contents = [ godon-bench-generic pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-bench-generic}/bin/godon-bench-generic" ];
      ExposedPorts = { "8090/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-bench-generic}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "RUST_LOG=info"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
    };
  };

in { inherit godon-bench-generic containerImage; }
