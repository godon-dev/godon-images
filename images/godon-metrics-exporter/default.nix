{ pkgs ? import <nixpkgs> {}, version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME" }:

let
  # Build the application using the same pattern as godon-api
  godon-metrics-exporter = pkgs.stdenv.mkDerivation {
    pname = "godon-metrics-exporter";
    version = version;
    
    src = ./.;
    
    # Disable strip phase to avoid build failures
    dontStrip = true;
    
    nativeBuildInputs = with pkgs; [
      cacert
      nim
      nimble
      gcc
      git
      openssl.dev
    ];
    
    buildInputs = with pkgs; [
      openssl
      pcre
      postgresql  # For libpq
    ];
    
    env = {
      SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      CURL_CA_BUNDLE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
    };
    
    configurePhase = ''
      export HOME=$TMPDIR
      export BUILD_VERSION="${builtins.getEnv "VERSION"}"
      echo "Building version: $BUILD_VERSION"
      if [ -z "$BUILD_VERSION" ]; then
        echo "❌ VERSION environment variable is required"
        exit 1
      fi
    '';

    buildPhase = ''
      echo "Using SSL setup in container..."
      echo "SSL_CERT_FILE: $SSL_CERT_FILE"
      echo "Certificate exists: $([ -f "$SSL_CERT_FILE" ] && echo "YES" || echo "NO")"
      
      # Following the godon-api build pattern
      nimble refresh -y
      nimble install --depsOnly -y
      
      # Build the metrics exporter using nimble (following prometheus_ss_exporter pattern)
      echo "Starting nimble build with maximum verbosity..."
      echo "Current directory contents:"
      ls -la || true
      echo "Current working directory: $(pwd)"
      
      # Capture all output to a log file and make it accessible even on failure
      exec 3>&1
      exec 1> >(tee nimble-build.log)
      exec 2>&1
      
      echo "=== NIMBLE BUILD DEBUG START ==="
      echo "Build directory: $(pwd)"
      echo "Environment variables:"
      env | grep -E "(NIM|BUILD|NIX)" || true
      echo "Files in current directory:"
      ls -la || true
      
      echo "=== RUNNING NIMBLE BUILD WITH FULL OUTPUT ==="
      set -x
      nimble build --verbose -d:release -d:metrics -d:version="$BUILD_VERSION" --threads:on
      exit_code=$?
      set +x
      
      echo "=== NIMBLE BUILD COMPLETE ==="
      echo "Exit code: $exit_code"
      
      # Copy logs to a temp location that might survive
      if [ -f "nimble-build.log" ]; then
        cp nimble-build.log /tmp/godon-metrics-exporter-build.log || true
        echo "Full build log saved to /tmp/godon-metrics-exporter-build.log"
      fi
    '';
    
    installPhase = ''
      mkdir -p $out/bin
      
      # Always save build logs to output for debugging
      if [ -f "nimble-build.log" ]; then
        mkdir -p $out/nix-build-logs
        cp nimble-build.log $out/nix-build-logs/nimble-build.log
        echo "Build logs saved to $out/nix-build-logs/nimble-build.log"
      fi
      
      echo "Looking for compiled binary..."
      echo "Current directory: $(pwd)"
      echo "Directory contents:"
      find . -name "exporter*" -type f -executable 2>/dev/null || true
      echo "All files:"
      find . -type f -name "*exporter*" || true
      
      # Install the exporter binary - follow prometheus_ss_exporter pattern
      if [ -f "bin/exporter" ]; then
        echo "Found binary in bin/exporter"
        cp bin/exporter $out/bin/godon-metrics-exporter
      elif [ -f "exporter" ]; then
        echo "Found binary in exporter"
        cp exporter $out/bin/godon-metrics-exporter
      else
        echo "Binary not found!"
        echo "Full directory listing:"
        ls -la
        exit 1
      fi
      
      # Make binary executable
      chmod +x $out/bin/*
      
      echo "✅ Installation completed successfully!"
      echo "Binary: "
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
      pkgs.postgresql  # For libpq runtime
    ];
    
    config = {
      Entrypoint = [ "${godon-metrics-exporter}/bin/godon-metrics-exporter" ];
      ExposedPorts = {
        "8089/tcp" = {};
      };
      Env = [
        "PATH=/bin:${godon-metrics-exporter}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "PORT=8089"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--port=8089" ];
    };
  };
  
in {
  inherit godon-metrics-exporter containerImage;
}