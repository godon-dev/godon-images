{ pkgs ? import <nixpkgs> {}, version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME", root ? builtins.getEnv "PROJECT_ROOT" }:

let
  # Copy shared windmill client to build context
  windmill-client = pkgs.runCommand "windmill-client" {} ''
    mkdir -p $out
    
    echo "🔍 Debug: Using mounted project-root directory"
    echo "🔍 Debug: Current working directory: $(pwd)"
    echo "🔍 Debug: Checking if /project-root is mounted:"
    if [ -d "/project-root" ]; then
      echo "✅ /project-root is mounted"
      echo "📁 Contents of /project-root:"
      ls -la /project-root | head -10
    else
      echo "❌ /project-root is not mounted"
      echo "🔍 Available mount points:"
      find / -maxdepth 1 -type d 2>/dev/null | head -10 || echo "Cannot check mount points"
    fi
    
    echo "🔍 Debug: Listing current directory:"
    ls -la . 2>/dev/null | head -5 || echo "Cannot list current directory"
    
    echo "🔍 Debug: Checking for shared windmill client:"
    if [ -d "/project-root/shared/windmill_client" ]; then
      echo "✅ Found shared windmill client at /project-root/shared/windmill_client"
      echo "📁 Contents of windmill_client:"
      ls -la "/project-root/shared/windmill_client" 2>/dev/null || echo "Cannot list windmill_client"
      cp -r "/project-root/shared/windmill_client"/* $out/
      echo "✅ Successfully copied shared windmill client"
    else
      echo "❌ Shared windmill client not found at /project-root/shared/windmill_client"
      echo "🔍 Debug: Checking /project-root exists: $([ -d "/project-root" ] && echo "YES" || echo "NO")"
      if [ -d "/project-root" ]; then
        echo "🔍 Available directories in /project-root:"
        find "/project-root" -maxdepth 2 -type d 2>/dev/null | head -10 || echo "Cannot search directories"
      fi
      echo "🔍 Debug: All available directories:"
      find / -name "*windmill*" -type d 2>/dev/null | head -10 || echo "No windmill directories found anywhere"
      exit 1
    fi
  '';

  # Build the application using the prometheus_ss_exporter pattern
  godon-api = pkgs.stdenv.mkDerivation {
    pname = "godon-api";
    version = version;
    
    src = ./src;
    
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
      
      # Copy shared windmill client to source directory (excluding .nimble to avoid conflicts)
      echo "Copying shared windmill client..."
      mkdir -p shared_windmill_client
      for file in ${windmill-client}/*; do
        if [[ "$(basename "$file")" != *.nimble ]]; then
          cp -r "$file" ./
        else
          # Copy .nimble files to a subdirectory where they won't interfere
          cp "$file" shared_windmill_client/
        fi
      done
    '';
    
    buildPhase = ''
      echo "Using documentation-based SSL setup in container..."
      echo "SSL_CERT_FILE: $SSL_CERT_FILE"
      echo "Certificate exists: $([ -f "$SSL_CERT_FILE" ] && echo "YES" || echo "NO")"
      
      # Following the exact prometheus_ss_exporter pattern
      nimble refresh
      nimble install --depsOnly
      
      # Build main application
      nimble build --verbose -d:release -d:BUILD_VERSION="$BUILD_VERSION" --threads:on --gc:orc -d:useStdLib
    '';
    
    installPhase = ''
      mkdir -p $out/bin
      
      echo "Looking for compiled binary..."
      echo "Current directory: $(pwd)"
      echo "Directory contents:"
      find . -name "godon_api*" -type f -executable 2>/dev/null || true
      echo "All files:"
      find . -type f -name "*godon*" || true
      
      # Install main binary - try multiple locations
      if [ -f "bin/godon_api" ]; then
        echo "Found binary in bin/godon_api"
        cp bin/godon_api $out/bin/
      elif [ -f "godon_api" ]; then
        echo "Found binary in godon_api"
        cp godon_api $out/bin/
      elif [ -f "godon_api.out" ]; then
        echo "Found binary in godon_api.out"
        cp godon_api.out $out/bin/godon_api
      elif [ -f "godon_api/godon_api" ]; then
        echo "Found binary in godon_api/godon_api"
        cp godon_api/godon_api $out/bin/
      else
        echo "Binary not found in any expected location!"
        echo "Full directory listing:"
        ls -la
        echo "nimble cache:"
        ls -la ~/.nimble/bin/ 2>/dev/null || true
        exit 1
      fi
      
        
      # Make all binaries executable
      chmod +x $out/bin/*
      
      echo "✅ Installation completed successfully!"
      echo "Main binaries: "
      ls -la $out/bin/
    '';
  };
  
  # Create container image using buildLayeredImage for better pseudo filesystem support
  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";
    
    # Use busybox for minimal base utilities and pseudo filesystem support
    fromImage = null;
    
    # Include runtime dependencies
    contents = [
      godon-api
      pkgs.cacert
      pkgs.busybox  # Provides basic utilities and pseudo filesystem support
      pkgs.curl    # For testing Windmill connectivity
    ];
    
    config = {
      Entrypoint = [ "${godon-api}/bin/godon_api" ];
      ExposedPorts = {
        "8080/tcp" = {};
      };
      Env = [
        "PATH=/bin:${godon-api}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "PORT=8080"
        "WINDMILL_BASE_URL=http://localhost:8001"
        "WINDMILL_API_BASE_URL=http://localhost:8001"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--help" ];  # Default command for container inspection
    };
  };
  
in {
  inherit godon-api containerImage;
}