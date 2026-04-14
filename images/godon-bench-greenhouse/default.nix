# godon-bench-greenhouse -- Nix Container Build
#
# Builds a minimal Docker image containing the greenhouse bench simulator.
# Uses the same build pattern as all other godon images (godon-api, godon-mcp, etc.)
# and is built by the shared build/build-container-nix.sh script.
#
# The image runs as user 1000:1000, exposes port 8090, and is configured via
# environment variables:
#   PORT=8090                 HTTP listen port
#   GREENHOUSE_SCENARIO=simple  simple (2 zones), medium (4), complex (6)
#
# CI: .github/workflows/godon-bench-greenhouse-ci.yml
# Release: .github/workflows/godon-bench-greenhouse-release.yml

{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME" }:

let

  rustPlatform = pkgs.rustPlatform;

  godon-bench-greenhouse = rustPlatform.buildRustPackage {
    pname = "godon-bench-greenhouse";
    version = version;

    src = ./.;

    cargoHash = "sha256-LvDxQSffGp5089pQ2npPp20jBP8flzF9ILv16OlX6kI=";

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    doCheck = false;

    buildPhase = ''
      echo "Building Rust godon-bench-greenhouse..."
      export HOME=$TMPDIR
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-bench-greenhouse $out/bin/godon-bench-greenhouse
      chmod +x $out/bin/godon-bench-greenhouse
      echo "Installation completed"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    # Minimal runtime: the binary, SSL certs (for any future HTTPS calls),
    # busybox (shell utilities), and curl (health checks)
    contents = [ godon-bench-greenhouse pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-bench-greenhouse}/bin/godon-bench-greenhouse" ];
      ExposedPorts = { "8090/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-bench-greenhouse}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "PORT=8090"
        "GREENHOUSE_SCENARIO=simple"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
    };
  };

in { inherit godon-bench-greenhouse containerImage; }
