feat(core): lay out inlay hints in RowLayout

Hints push the following text right, so carets, hits, widths and the
collapsed header have to count their cells. Each edge picks a side of the
hints at an offset.
