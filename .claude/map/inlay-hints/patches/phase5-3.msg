feat(iced): inlay hints in CodeEditor, fetched on a debounce

The editor decides when hints are due: at once when turned on or after a
load, 300 ms after typing pauses, and 75 ms after a scroll leaves the fetched
window, capped at 300 ms so a scrollbar drag still fetches as it goes. Each
answer, tooltip and gesture lands only under the ticket it was asked with.
