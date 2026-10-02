feat(iced)!: ticketed async requests with editor-side abandonment

A revision stamp let replies land for requests the user had already walked
away from, and could not tell two requests at one revision apart. Each async
service now awaits one ticket, every way of abandoning a request clears it,
and async completion keeps the provider path's dismissal and refilter rules.
The request types move to scrive-core so a client can build them without iced.
