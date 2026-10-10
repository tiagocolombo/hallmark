---
id: L-0011
kind: rule
scope: ["src/commands/new.rs"]
lens: correctness
source: ["H-0004/F4"]
status: proposed
used: 0
---
# In the follow-up request, place the user's text and the review feedback before the CI log excerpts, and cap the excerpts in total, so the request's final truncation cannot cut human text.

The request is cut at 12,000 characters from the end, so excerpts placed earlier push the user's instruction and review comments out of the request. Keep the CI section last with a shared cap across all failing checks, and test with several long failing logs plus a user instruction.
