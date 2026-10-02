feat(core): completion item filter text, extra edits and a shared filter predicate

Language-server items filter on a text other than their label, carry edits
elsewhere (auto-imports) and can ask for signature help once accepted. The
popup and any later list reuse must agree on one filter rule, and an
externally produced list must not reopen a popup dismissed with Escape.
