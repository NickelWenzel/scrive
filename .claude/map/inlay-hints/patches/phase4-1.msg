fix(iced): expand a run's tabs from its own raw cell

expand_tabs counted tab stops from cell 0 of each run while the painters
place runs at their true start cell, so a tab in a run that began off a
stop painted too wide and the glyphs after it drifted right of the caret.
