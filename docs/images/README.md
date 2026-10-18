# Images

## Inside the Hall

`inside-the-hall.gif` and `inside-the-hall-dark.gif` (in the README and on
the site) are 30 seconds of the live floor plan in
[`design.html`](../design.html). Change the plan there, then capture it again:

```sh
mkdir -p target/capture && cd target/capture
npm init -y && npm i playwright-core@1.56.1
cp ../../docs/images/capture.js .
node capture.js ../../docs/design.html frames/light light 15 30
node capture.js ../../docs/design.html frames/dark dark 15 30
nix shell nixpkgs#gifski --command sh -c '
  gifski --fps 15 --quality 80 --width 1000 -o ../../docs/images/inside-the-hall.gif frames/light/*.png &&
  gifski --fps 15 --quality 80 --width 1000 -o ../../docs/images/inside-the-hall-dark.gif frames/dark/*.png'
```

`capture.js` drives Google Chrome through Playwright (pass another Chrome as
its sixth argument). The animation is deterministic: the same page gives the
same frames.

## The terminal app

`tui-board.gif`, `tui-office.gif`, `tui-review.gif` and `tui-light.gif` are
recordings of this checkout's `hall` against a fictional repo, made with
[VHS](https://github.com/charmbracelet/vhs). `tui-light.gif` is one tour of
the board, an item, your office, findings, a diff and usage in a light theme
(`tapes/theme-light.tape`); the README and the site show it in light mode and
`tui-board.gif` in dark mode. [`tui-demo/`](tui-demo/) holds
everything: an example (`examples/hall-demo.rs`, not a separate package) that seeds the repo's items and journals and plays
the daemon's part while the board is recorded (`hall-demo seed`, `animate`,
`idle`), the environment it runs in (no model or GitHub calls, a private
`HOME`), and one tape per GIF.

```sh
cargo build --bins --example hall-demo              # hall, and the seeder
VHS=$(nix build --no-link --print-out-paths nixpkgs#vhs)/bin/vhs
cd docs/images/tui-demo
for t in board office review light; do
  ./reseed.sh && "$VHS" tapes/$t.tape        # writes ../tui-$t.gif
  pkill -f "hall-demo (animate|idle)"
done
```

`ACME` picks where the fictional repos live (default `/tmp/acme`). When a
journal event or a view changes, update `examples/hall-demo.rs` or the tapes to match.

## The web app

`hall-web.gif` is a tour of `hall web` in a light theme over the same
fictional repos: the live floor, an item approved and shipped while you
watch, its findings and diff, a question answered from your office, and
usage. [`tui-demo/web/record.js`](tui-demo/web/record.js) drives Google
Chrome through Puppeteer (`CHROME` picks another), draws a pointer, starts
`hall-demo animate` once the page is up so the items move on cue, and
resamples its screenshots to a steady 10 fps.

```sh
cargo build --bins --example hall-demo
nix shell nixpkgs#gifski --command docs/images/tui-demo/record-web.sh   # writes hall-web.gif
```

When the page's layout changes, update the selectors in `record.js`.
