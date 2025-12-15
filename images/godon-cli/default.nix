{ version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME"
, buildTests ? builtins.getEnv "BUILD_TESTS" }:

let
  # Use the direct flake approach as recommended in the documentation
  system = builtins.currentSystem or "x86_64-linux";

  # Use the version parameter to select the appropriate CLI tag to pull
  # VERSION specifies which CLI container tag to use
  godon-cli-flake = builtins.getFlake "github:godon-dev/godon-cli/${version}";

  # Get the package and nixpkgs from the flake's outputs to avoid duplication
  pkgs = godon-cli-flake.inputs.nixpkgs.legacyPackages.${system};
  godon-cli = godon-cli-flake.packages.${system}.godon-cli { version = version; };

  # Create a minimal runtime package with just the binary
  godon-cli-runtime = pkgs.runCommand "godon-cli-runtime" { } ''
    mkdir -p $out/bin
    cp ${godon-cli}/bin/godon_cli $out/bin/
  '';

  # Create container image using buildLayeredImage
  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    # Use busybox for minimal base utilities
    fromImage = null;

    # Include only runtime dependencies
    contents = [
      godon-cli-runtime # Just the binary, no build deps
      pkgs.cacert
      pkgs.busybox # Provides basic utilities
      pkgs.curl # For API calls
    ];

    config = {
      Entrypoint = [ "${godon-cli-runtime}/bin/godon_cli" ];
      Env = [ "PATH=/bin:${godon-cli-runtime}/bin" "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--help" ]; # Default command for container inspection
      # Set creation time to avoid epoch timestamp display
      Created = "2025-12-15T00:00:00Z";
    };
  };

in { inherit godon-cli containerImage; }
