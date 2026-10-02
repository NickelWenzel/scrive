feat(lsp): normalized document URI keys

Servers and hosts spell one file several ways (`%3A`, `localhost`, drive-letter case), and
lsp-types' `Uri` compares raw strings, so documents need one normalized key to register, look up
and send under.
