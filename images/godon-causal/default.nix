{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME" }:

let
  rustPlatform = pkgs.rustPlatform;

  godon-causal = rustPlatform.buildRustPackage {
    pname = "godon-causal";
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
      echo "Building godon-causal..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-causal $out/bin/godon-causal
      chmod +x $out/bin/godon-causal
      echo "Installation completed"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    contents = [ godon-causal pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-causal}/bin/godon-causal" ];
      ExposedPorts = { "8091/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-causal}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "RUST_LOG=info"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
    };
  };

in { inherit godon-causal containerImage; }
