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
    # Repository URLs and versions for build-time cloning
    CONTROLLER_REPO_URL = "https://github.com/godon-dev/godon-controller.git";
    BREEDER_REPO_URL = "https://github.com/godon-dev/godon-breeders.git";
    GODON_BUILD_VERSION = let envVar = builtins.getEnv "VERSION"; in
                           if envVar != "" then envVar else "main";
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
      echo "Using documentation-based SSL setup in container..."
      echo "SSL_CERT_FILE: $SSL_CERT_FILE"
      echo "Certificate exists: $([ -f "$SSL_CERT_FILE" ] && echo "YES" || echo "NO")"

      # Build Nim application
      nimble refresh
      nimble install --depsOnly

      echo "Building godon-seeder binary..."
      nim c --path:"/shared" -d:release -d:BUILD_VERSION="$BUILD_VERSION" --threads:on --gc:orc -d:useStdLib godon_seeder.nim
    '';
    installPhase = ''
      mkdir -p $out/bin

      # Pre-clone repositories for faster container startup
      if [ "$BUILD_VERSION" != "test-local" ]; then
        echo "Pre-cloning repositories for faster startup..."
        mkdir -p $out/var/lib/godon

        echo "Cloning godon-controller repository from $CONTROLLER_REPO_URL"
        git clone --depth 1 --branch "$GODON_BUILD_VERSION" "$CONTROLLER_REPO_URL" $out/var/lib/godon/godon-controller || echo "⚠️  godon-controller clone failed"

        echo "Cloning godon-breeders repository from $BREEDER_REPO_URL"
        git clone --depth 1 --branch "$GODON_BUILD_VERSION" "$BREEDER_REPO_URL" $out/var/lib/godon/godon-breeders || echo "⚠️  godon-breeders clone failed"

        echo "✅ Pre-cloned repositories successfully in $out/var/lib/godon"
      else
        echo "Test build detected, creating directory structure..."
        mkdir -p $out/var/lib/godon
      fi

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
      pkgs.git     # For git pull and checkout operations at runtime
      pkgs.bash    # For running orchestration script
      (pkgs.writeTextDir "etc/passwd" "root:x:0:0:root:/root:/bin/sh\ngodon:x:1000:1000:godon:/var/lib/godon:/bin/sh\n")
      (pkgs.writeTextDir "etc/group" "root:x:0:\ngodon:x:1000:\n")
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
        "GODON_DIR=/var/lib/godon"
      ];
      WorkingDir = "/var/lib/godon";
      User = "1000:1000";
      Cmd = [ "--help" ];  # Default command for container inspection
    };
    # Use fakeRootCommands to set ownership of directories
    fakeRootCommands = ''
      mkdir -p var/lib/godon
      chown -R 1000:1000 var/lib/godon
    '';
  };

in {
  inherit godon-seeder containerImage;
}
