{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME" }:

let

  rustPlatform = pkgs.rustPlatform;

  godon-bench-microgrid = rustPlatform.buildRustPackage {
    pname = "godon-bench-microgrid";
    version = version;

    src = ./.;

    cargoHash = "sha256-+Nz4Fc1HrSNvw7vi4D8E8u6yh0W7NDoI6kPs7vkl0fA=";

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    doCheck = false;

    buildPhase = ''
      echo "Building Rust godon-bench-microgrid..."
      export HOME=$TMPDIR
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cp target/release/godon-bench-microgrid $out/bin/godon-bench-microgrid
      chmod +x $out/bin/godon-bench-microgrid
      echo "Installation completed"
      ls -la $out/bin/
    '';
  };

  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    contents = [ godon-bench-microgrid pkgs.cacert pkgs.busybox pkgs.curl ];

    config = {
      Entrypoint = [ "${godon-bench-microgrid}/bin/godon-bench-microgrid" ];
      ExposedPorts = { "8090/tcp" = { }; };
      Env = [
        "PATH=/bin:${godon-bench-microgrid}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "PORT=8090"
        "MICROGRID_SEED=42"
        "COUPLING_NEIGHBORS="
        "COUPLING_FACTOR=0.0"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
    };
  };

in { inherit godon-bench-microgrid containerImage; }
