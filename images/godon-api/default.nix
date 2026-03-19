{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME"
, root ? builtins.getEnv "PROJECT_ROOT" }:

let

  rustPlatform = pkgs.rustPlatform;

  godon-api = rustPlatform.buildRustPackage {
    pname = "godon-api";
    version = version;

    src = ./.;

    cargoHash = "sha256-ogoeZCr+pKTLOGo0SDyFjtKpwepkNt5QgUWdPFtYJQg=";

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    buildInputs = with pkgs; [ openssl ];

    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    doCheck = false;

    buildPhase = ''
      echo "Building Rust godon-api..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-api $out/bin/godon-api
      chmod +x $out/bin/godon-api
      echo "✅ Installation completed successfully!"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    contents = [ godon-api pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-api}/bin/godon-api" ];
      ExposedPorts = { "8080/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-api}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "PORT=8080"
        "WINDMILL_BASE_URL=http://localhost:8000/api"
        "WINDMILL_WORKSPACE=godon"
        "WINDMILL_FOLDER=controller"
        "WINDMILL_EMAIL=admin@windmill.dev"
        "WINDMILL_PASSWORD=changeme"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--help" ];
    };
  };

in { inherit godon-api containerImage; }
