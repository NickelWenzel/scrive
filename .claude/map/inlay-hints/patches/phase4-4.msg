fix(iced): underline a hint at the start of a squiggle's continuation row

A hint at column 0 of a continuation row lies strictly inside the diagnostic,
so it belongs under the squiggle; starting the row at Start left it out.
Headless tests pin every overlay's edge on a hinted row.
