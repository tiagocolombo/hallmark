# Matt Pocock's skills

These skills are by **Matt Pocock**, from
[github.com/mattpocock/skills](https://github.com/mattpocock/skills), and are
used under the MIT license in [LICENSE](LICENSE). Thank you, Matt.

They are copied here unchanged from commit
`49dd158d1076134a641b33efb035946536778336` (9 October 2026) and embedded in
`hall`, so the extension works offline and every run of one `hall` version
reads the same text. Hallmark doesn't edit them: how each one fits a
Hallmark role is said in `src/extensions.rs`, next to the text.

| Skill | Upstream path | Used by |
| --- | --- | --- |
| `to-tickets` | `skills/engineering/to-tickets/` | planner (the Front Desk) |
| `tdd` | `skills/engineering/tdd/` | developer, fixer |
| `diagnosing-bugs` | `skills/engineering/diagnosing-bugs/` | developer, fixer |
| `codebase-design` | `skills/engineering/codebase-design/` | developer |
| `code-review` | `skills/engineering/code-review/` | reviewer |

Turn them on with `hall extensions enable mattpocock`; `hall extensions`
lists what each role gets. See [docs/commands.md](../../docs/commands.md)
and ADR [0038](../../docs/decisions/0038-extensions.md).

## Updating the pin

1. `git clone https://github.com/mattpocock/skills` and note the commit.
2. Copy each skill's `*.md` files over the ones here, and `LICENSE`.
3. Set `COMMIT` in `src/extensions.rs` and the commit above to the new one.
4. Read the diff: a skill that changed shape may need a new note in
   `src/extensions.rs`. Then run the tests.
