# Godon Images

Container images built with Nix for reproducible, isolated builds.

## Images

| Image | Language | Port | Purpose |
|-------|----------|------|---------|
| `godon-api` | Rust (Axum) | 8080 | REST API for managing optimizer breeders and credentials |
| `godon-cli` | Rust | - | CLI tool for interacting with godon-api |
| `godon-metrics-exporter` | Rust (Hyper) | 8089 | Prometheus metrics proxy from Push Gateway |
| `godon-seeder` | Rust | - | Deploys controller/breeder scripts to Windmill |
| `godon-mcp` | Rust (Axum) | 3001 | MCP server exposing godon-api as tool interface for LLM agents |

## Structure

```
godon-images/
├── build/                           # Shared build infrastructure
│   ├── Dockerfile.nix-builder       # Isolated Nix builder container
│   └── build-container-nix.sh       # Build script (uses dockerTools.buildLayeredImage)
├── images/                          # Service images (one directory per image)
│   ├── godon-api/
│   ├── godon-cli/
│   ├── godon-metrics-exporter/
│   ├── godon-seeder/
│   └── godon-mcp/
├── shared/                          # Shared test infrastructure
│   └── tests/                       # Windmill test stack, stub scripts, test data
```

## Building

```bash
cd images/<image-name>
../../build/build-container-nix.sh --version <version> --name <image-name>
```

## Releasing

Push a tag in the format `<image-name>-X.Y.Z` (e.g., `godon-mcp-0.0.1`). The release workflow builds and pushes to `ghcr.io/godon-dev/<image-name>`.

## Architecture

- **Nix-based builds**: Reproducible, minimal container images via `dockerTools.buildLayeredImage`
- **One image per concern**: Each service is a separate container with its own CI/release pipeline
- **Shared build system**: All images use the same builder container and build script

