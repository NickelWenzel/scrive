feat(core): request tickets

An async reply stamped only with a revision cannot tell two requests at the
same revision apart, nor a request the user has since abandoned. A ticket
names one request, and only a per-editor counter mints them.
