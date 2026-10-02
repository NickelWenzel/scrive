refactor(iced): draw and hit-test through one Rows per pass

One view per widget pass builds each row's layout once per frame, and every
projection names the edge it means, so hint-aware layout lands without
revisiting the call sites.
