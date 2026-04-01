{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME"
, root ? builtins.getEnv "PROJECT_ROOT" }:

let

  rustPlatform = pkgs.rustPlatform;

  godon-mcp = rustPlatform.buildRustPackage {
    pname = "godon-mcp";
    version = version;

    src = ./.;

    cargoHash = "sha256-yw88FdBrpi9ePaT6uTjT5bmDT7MLfa3UN26eU90LfN0=";

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    buildInputs = with pkgs; [ openssl ];

    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    doCheck = false;

    buildPhase = ''
      echo "Building Rust godon-mcp..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-mcp $out/bin/godon-mcp
      chmod +x $out/bin/godon-mcp
      echo "Installation completed successfully!"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    contents = [ godon-mcp pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-mcp}/bin/godon-mcp" ];
      ExposedPorts = { "3001/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-mcp}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "PORT=3001"
        "GODON_API_HOSTNAME=godon-api"
        "GODON_API_PORT=8080"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--help" ];
    };
  };

in { inherit godon-mcp containerImage; }
