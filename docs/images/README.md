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

`tui-board.gif`, `tui-office.gif` and `tui-review.gif` are recordings of
`hall` against a fictional demo repo, made with
[VHS](https://github.com/charmbracelet/vhs).
