{ pkgs ? import <nixpkgs> {}, version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME", buildTests ? builtins.getEnv "BUILD_TESTS" }:

let
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
      BUILD_TESTS = buildTests;
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
      echo "BUILD_TESTS: $BUILD_TESTS"
      
      # Following the exact prometheus_ss_exporter pattern
      nimble refresh
      nimble install --depsOnly
      
      # Build main application
      nimble build --verbose -d:release -d:BUILD_VERSION="$BUILD_VERSION" --threads:on --gc:orc -d:useStdLib
      
      # Conditionally build test executables for in-container testing
      if [ "$BUILD_TESTS" = "true" ]; then
        echo "Building test executables..."
        mkdir -p tests
        
        # Copy test files to build directory
        cp -r ../tests/* tests/ 2>/dev/null || true
        
        # Build unit tests
        if [ -f "tests/test_handlers.nim" ]; then
          echo "Building unit tests..."
          nim c -d:testing --hints:off -o:tests/test_handlers tests/test_handlers.nim || echo "Warning: Unit test build failed"
        fi
        
        # Build integration tests  
        if [ -f "tests/test_integration.nim" ]; then
          echo "Building integration tests..."
          nim c -d:testing --hints:off -o:tests/test_integration tests/test_integration.nim || echo "Warning: Integration test build failed"
        fi
      else
        echo "Skipping test build (BUILD_TESTS=false)"
      fi
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
      
      # Conditionally install test binaries if they exist
      if [ "$BUILD_TESTS" = "true" ]; then
        echo "Installing test binaries..."
        mkdir -p $out/tests
        
        if [ -f "tests/test_handlers" ]; then
          echo "Installing unit test binary"
          cp tests/test_handlers $out/tests/
          chmod +x $out/tests/test_handlers
        fi
        
        if [ -f "tests/test_integration" ]; then
          echo "Installing integration test binary"
          cp tests/test_integration $out/tests/
          chmod +x $out/tests/test_integration
        fi
        
        # Install test scripts and data
        if [ -d "tests" ]; then
          cp -r tests/*.yaml $out/tests/ 2>/dev/null || true
        fi
        
        # Install container test runner
        if [ -f "../container_test_runner.sh" ]; then
          echo "Installing container test runner"
          cp ../container_test_runner.sh $out/tests/run_tests
          chmod +x $out/tests/run_tests
        fi
      else
        echo "Skipping test installation (BUILD_TESTS=false)"
      fi
      
      # Make all binaries executable
      chmod +x $out/bin/*
      chmod +x $out/tests/* 2>/dev/null || true
      
      echo "✅ Installation completed successfully!"
      echo "Main binaries: "
      ls -la $out/bin/
      echo "Test binaries: "
      ls -la $out/tests/ 2>/dev/null || echo "No test binaries found"
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
        "PATH=/bin:${godon-api}/bin:${godon-api}/tests"
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