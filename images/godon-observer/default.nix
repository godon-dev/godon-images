{ pkgs ? import <nixpkgs> { }, version ? builtins.getEnv "VERSION"
, imageName ? builtins.getEnv "IMAGE_NAME"
, root ? builtins.getEnv "PROJECT_ROOT" }:

let
  rustPlatform = pkgs.rustPlatform;

  godon-observer = rustPlatform.buildRustPackage {
    pname = "godon-observer";
    version = version;

    # The repo root is the source: the ledger-reader crate lives in
    # shared/ and both readers depend on it by path. In the builder
    # container the root arrives as the mounted /project-root (the
    # script passes --argstr root); locally it falls back to the
    # relative tree. Filtered — no target/ trees, no droppings.
    src = pkgs.lib.cleanSourceWith {
      src = (if root == "" then ../../. else /. + root);
      filter = path: type:
        with pkgs.lib;
        ! (type == "directory" && baseNameOf path == "target")
        && ! (type == "directory" && hasPrefix "." (baseNameOf path))
        && ! (hasSuffix ".nix" path)
        && ! (hasSuffix ".md" path)
        && ! (hasPrefix "bench_" (baseNameOf path));
    };

    # Build inside the image's own crate directory.
    buildAndTestSubdir = "images/godon-observer";

    cargoLock.lockFile = ./Cargo.lock;

    # nixpkgs' cargo hook validates Cargo.lock at the SOURCE ROOT;
    # with the widened source the crate lives one level down — copy
    # the lock up so the check passes.
    postPatch = ''
      cp images/godon-observer/Cargo.lock Cargo.lock
    '';

    nativeBuildInputs = with pkgs; [ cacert pkg-config ];

    buildInputs = with pkgs; [ openssl ];

    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    doCheck = true;

    checkPhase = ''
      echo "Running unit tests..."
      export HOME=$TMPDIR
      cd "$NIX_BUILD_TOP/$sourceRoot"/images/godon-observer
      cargo test 2>&1
    '';

    buildPhase = ''
      echo "Building godon-observer..."
      export HOME=$TMPDIR
      export BUILD_VERSION="${version}"
      cd "$NIX_BUILD_TOP/$sourceRoot"/images/godon-observer
      cargo build --release
    '';

    installPhase = ''
      mkdir -p $out/bin
      cd "$NIX_BUILD_TOP/$sourceRoot"/images/godon-observer
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
