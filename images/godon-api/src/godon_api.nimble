# Package
version       = "0.1.0"
author        = "Matthias Tafelmeier"
description   = "Godon Control API - RESTful API for managing optimizer breeders"
license       = "AGPL-3.0"
srcDir        = "."
installExt    = @["nim"]

# Dependencies
requires "nim >= 2.0.0"
requires "jester >= 0.5.0"
requires "jsony >= 1.1.5"

skipFiles = @[]
bin           = @["godon_api"]
