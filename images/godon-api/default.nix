{ pkgs ? import <nixpkgs> {}, version ? builtins.getEnv "VERSION", imageName ? builtins.getEnv "IMAGE_NAME" }:

let
  lib = pkgs.lib;
  
  godon-api = pkgs.buildNimPackage {
    pname = "godon-api";
    version = version;
    src = ./.;

    nativeBuildInputs = [ openssl ];
    buildInputs = [ pcre ];

    nimRelease = true;
    nimDefine = "release";
  };
  
  containerImage = pkgs.dockerTools.buildLayeredImage {
    name = "${imageName}";
    tag = "${version}";
    
    fromImage = null;
    
    contents = [
      godon-api
      pkgs.cacert
      pkgs.busybox
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
      ];
      WorkingDir = "/app";
      User = "1000:1000";
    };
  };
  
in {
  inherit godon-api containerImage;
}