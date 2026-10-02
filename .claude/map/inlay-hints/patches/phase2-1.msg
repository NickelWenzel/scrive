refactor(core)!: project core geometry through row_layout::Rows

A projection that bundles folds, buffer, inlay store and tab width can't forget
hints once they render. Visibility probes keep a hint-free `FoldMap::renders`;
box, add-caret and arrow paths read the cached fold map instead of rebuilding it.
