{ pkgs ? import <nixpkgs> {}, version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME" }:

let
  # Rust toolchain
  rustPlatform = pkgs.rustPlatform;

  # Build the Rust application
  godon-metrics-exporter = rustPlatform.buildRustPackage {
    pname = "godon-metrics-exporter";
    version = version;

    src = ./.;

    cargoHash = "sha256-vEmFYTvPED2gzse9p1v2rxnKr9MNRVMFgp5x/iCl1ME=";

    nativeBuildInputs = with pkgs; [
      cacert
      pkg-config
    ];

    buildInputs = with pkgs; [
      openssl
    ];

    # Set environment variables for build
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    # Disable tests since we're building a container image
    doCheck = false;

    buildPhase = ''
      echo "Building Rust metrics exporter..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-metrics-exporter $out/bin/godon-metrics-exporter
      chmod +x $out/bin/godon-metrics-exporter
      echo "✅ Installation completed successfully!"
      ls -la $out/bin/
    '';
  };
  
  # Create container image using buildLayeredImage
  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    # Include runtime dependencies
    contents = [
      godon-metrics-exporter
      pkgs.cacert
      pkgs.busybox
      pkgs.curl
    ];

    config = {
      Entrypoint = [ "${godon-metrics-exporter}/bin/godon-metrics-exporter" ];
      ExposedPorts = {
        "8089/tcp" = {};
      };
      Env = [
        "PATH=/bin:${godon-metrics-exporter}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "RUST_LOG=info"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--port=8089" ];
    };
  };

in {
  inherit godon-metrics-exporter containerImage;
}