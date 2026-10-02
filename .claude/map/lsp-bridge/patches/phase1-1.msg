feat(core): snapshot point, chunk and clip access

A consumer off the UI thread (position-encoding conversion for a language
server) needs point conversion, clipping and allocation-free text walks on a
frozen snapshot, not just on the live buffer.
