chore: share internal crate versions through workspace dependencies

Declaring scrive-core once in the root manifest means a release bump edits
one file instead of every dependent crate's manifest.
