# Godon Images

Container images built with Nix for reproducible, isolated builds.

## Current Structure

```
godon-images/
├── build/                           # Shared build infrastructure
│   ├── Dockerfile.nix-builder       # Isolated Nix builder container
│   └── build-container-nix.sh        # Build script (uses dockerTools.buildLayeredImage)
├── images/                          # Service images
│   ├── godon-api/                   # Godon API service (Nim) - source files need to be restored
│   ├── godon-metrics-exporter/      # Metrics exporter service (Nim)
│   ├── godon-seeder/                # Seeder service
│   └── godon-dask/                  # Dask service
```

## Building Images

```

## Architecture

- **Isolated Nix builds**: Each service builds in an isolated container to avoid Nix sandboxing issues
- **Reproducible containers**: Nix ensures exact same dependencies everywhere  
- **Simple structure**: Focus on container building without unnecessary complexity
- **Extensible**: Easy to add new services following the same pattern
- **Mixed approach**: New services use Nix, existing services keep their current build system

