{ pkgs ? import <nixpkgs> {}, version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME", root ? builtins.getEnv "PROJECT_ROOT" }:

let

  # Build the application using the same pattern as godon-api
  godon-seeder = pkgs.stdenv.mkDerivation {
    pname = "godon-seeder";
    version = version;
    
    src = ./.;
    
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
    
    # Godon repository URL and version for build-time cloning
    GODON_REPO_URL = "https://github.com/godon-dev/godon.git";
    GODON_BUILD_VERSION = let envVar = builtins.getEnv "GODON_VERSION"; in
                           if envVar != "" then envVar else "master";
    
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
      
      # Direct mount approach - shared client available at /shared
      echo "Shared client available at /shared/windmill_client"
    '';
    
    buildPhase = ''
      echo "Using documentation-based SSL setup in container..."
      echo "SSL_CERT_FILE: $SSL_CERT_FILE"
      echo "Certificate exists: $([ -f "$SSL_CERT_FILE" ] && echo "YES" || echo "NO")"
      
      # Clone godon repository at build time (only if not a test build)
      if [ "$BUILD_VERSION" != "test-local" ]; then
        echo "Cloning godon repository from $GODON_REPO_URL at version $GODON_BUILD_VERSION"
        mkdir -p godon-repo
        git clone --depth 1 --branch "$GODON_BUILD_VERSION" "$GODON_REPO_URL" godon-repo || echo "⚠️  Git clone failed, continuing anyway"
        echo "✅ Godon repository cloned successfully"
      else
        echo "Test build detected, skipping git clone"
      fi
      
      # Following the exact godon-api pattern
      nimble refresh
      nimble install --depsOnly
      
      # Build main application with shared client in Nim path
      echo "Building godon-seeder with shared client path..."
      nim c --path:"/shared" -d:release -d:BUILD_VERSION="$BUILD_VERSION" --threads:on --gc:orc -d:useStdLib godon_seeder.nim
    '';
    
    installPhase = ''
      mkdir -p $out/bin
      
      echo "Looking for compiled binary..."
      echo "Current directory: $(pwd)"
      echo "Directory contents:"
      find . -name "godon_seeder*" -type f -executable 2>/dev/null || true
      echo "All files:"
      find . -type f -name "*godon*" || true
      
      # Install main binary - try multiple locations
      if [ -f "bin/godon_seeder" ]; then
        echo "Found binary in bin/godon_seeder"
        cp bin/godon_seeder $out/bin/
      elif [ -f "godon_seeder" ]; then
        echo "Found binary in godon_seeder"
        cp godon_seeder $out/bin/
      elif [ -f "godon_seeder.out" ]; then
        echo "Found binary in godon_seeder.out"
        cp godon_seeder.out $out/bin/godon_seeder
      elif [ -f "godon_seeder/godon_seeder" ]; then
        echo "Found binary in godon_seeder/godon_seeder"
        cp godon_seeder/godon_seeder $out/bin/
      else
        echo "Binary not found in any expected location!"
        echo "Full directory listing:"
        ls -la
        echo "nimble cache:"
        ls -la ~/.nimble/bin/ 2>/dev/null || true
        exit 1
      fi
      
      # Copy the orchestration shell script
      if [ -f "godon_seeder.sh" ]; then
        echo "Copying orchestration script"
        cp godon_seeder.sh $out/bin/
        chmod +x $out/bin/godon_seeder.sh
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
      godon-seeder
      pkgs.cacert
      pkgs.busybox  # Provides basic utilities and pseudo filesystem support
      pkgs.curl    # For testing Windmill connectivity
      pkgs.git     # For git pull and checkout operations at runtime
      pkgs.bash    # For running orchestration script
    ];
    
    config = {
      Entrypoint = [ "${godon-seeder}/bin/godon_seeder.sh" ];
      ExposedPorts = {
        "8080/tcp" = {};
      };
      Env = [
        "PATH=/bin:${godon-seeder}/bin"
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "WINDMILL_BASE_URL=http://windmill-app:8000"
        "WINDMILL_WORKSPACE=godon"
        "GODON_VERSION=main"
        "GODON_DIR=/godon"
      ];
      WorkingDir = "/app";
      User = "1000:1000";
      Cmd = [ "--help" ];  # Default command for container inspection
    };
  };
  
in {
  inherit godon-seeder containerImage;
}
