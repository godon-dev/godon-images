# Package
version       = "0.1.0"
author        = "Matthias Tafelmeier"
description   = "Godon Seeder - Deploy Godon optimization components to Windmill"
license       = "AGPL-3.0"
srcDir        = "."
installExt    = @["nim"]

# Dependencies
requires "nim >= 2.0.0"

skipFiles = @[]
bin           = @["godon_seeder"]
