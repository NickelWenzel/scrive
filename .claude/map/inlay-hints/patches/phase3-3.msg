feat(core): step box selection by caret stop

A corner stepping by cell stalls on one offset for as many presses as a hint
is wide. Stepping by caret stop crosses a hint, a tab or a chip in one press,
and past the line end it still steps by cell.
