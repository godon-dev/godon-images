{ version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME"
, buildTests ? builtins.getEnv "BUILD_TESTS" }:

let
  # Use the direct flake approach as recommended in the documentation
  system = builtins.currentSystem or "x86_64-linux";

  # Use the version parameter to select the robots tag to pull
  godon-robots-flake = builtins.getFlake "github:godon-dev/godon-robots/${version}";

  # Get the package and nixpkgs from the flake's outputs to avoid duplication
  pkgs = godon-robots-flake.inputs.nixpkgs.legacyPackages.${system};
  godon-engine = godon-robots-flake.packages.${system}.godon-engine { version = version; };

  # Create a minimal runtime package with just the binary
  godon-engine-runtime = pkgs.runCommand "godon-engine-runtime" { } ''
    mkdir -p $out/bin
    cp ${godon-engine}/bin/godon-engine $out/bin/
  '';

  # Binary + busybox: the windmill sandbox rail's wrapper contract
  # runs a script BODY via the image's /bin/sh -c (the payload
  # wrapper that passes job args to the binary) — so the shell
  # returns to the binary-only image (the holder's word, the
  # controller organ's same shape). Nothing else changes: no
  # cacert, no curl, nothing shells out beyond the wrapper itself.
  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    contents = [
      godon-engine-runtime # Just the engine binary, runtime libs ride the closure
      pkgs.busybox # /bin/sh for the sandbox rail's wrapper body
    ];

    config = {
      # The component dispatch: argv[1] names the component
      # (worker | recon-http | recon-prometheus | effectuation-http |
      # effectuation-ssh), argv[2] carries the args JSON
      Entrypoint = [ "${godon-engine-runtime}/bin/godon-engine" ];
      Env = [ "PATH=${godon-engine-runtime}/bin" ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ ];
      Created = "2025-12-15T00:00:00Z";
    };
  };

in { inherit godon-engine containerImage; }
