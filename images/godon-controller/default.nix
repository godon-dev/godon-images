{ version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME"
, buildTests ? builtins.getEnv "BUILD_TESTS" }:

let
  # Use the direct flake approach as recommended in the documentation
  system = builtins.currentSystem or "x86_64-linux";

  # Use the version parameter to select the controller tag to pull
  godon-controller-flake = builtins.getFlake "github:godon-dev/godon-controller/${version}";

  # Get the package and nixpkgs from the flake's outputs to avoid duplication
  pkgs = godon-controller-flake.inputs.nixpkgs.legacyPackages.${system};
  godon-controller = godon-controller-flake.packages.${system}.godon-controller { version = version; };

  # Create a minimal runtime package with just the binary
  godon-controller-runtime = pkgs.runCommand "godon-controller-runtime" { } ''
    mkdir -p $out/bin
    cp ${godon-controller}/bin/godon-controller $out/bin/
  '';

  # Binary-only image, no shell needed: empty-body sandbox scripts run the
  # binary as the entrypoint, and the binary serves its own arguments through
  # the platform API (WM_JOB_PATH routes, jobs_u/get_args carries the payload
  # - the robots lane's receipts, adopted). No curl, no cacert, no busybox.
  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";

    fromImage = null;

    contents = [
      godon-controller-runtime # Just the binary, runtime libs ride the closure
    ];

    config = {
      Entrypoint = [ "${godon-controller-runtime}/bin/godon-controller" ];
      Env = [ "PATH=${godon-controller-runtime}/bin" ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ ];
      Created = "2025-12-15T00:00:00Z";
    };
  };

in { inherit godon-controller containerImage; }
