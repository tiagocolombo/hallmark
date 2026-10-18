// Captures the Inside the Hall floor plan from docs/design.html as PNG
// frames, for gifski to turn into docs/images/inside-the-hall*.gif. See
// docs/images/README.md.
//
// Usage: node capture.js <design.html> <out-dir> <light|dark> <fps> <seconds> [chrome]
//
// Deterministic: Math.random is seeded and requestAnimationFrame runs on a
// clock this script advances, one frame at a time.
const { chromium } = require("playwright-core");
const path = require("path");
const fs = require("fs");

const [page_, outDir, scheme = "light", fpsArg = "15", secArg = "30", chrome] = process.argv.slice(2);
const fps = +fpsArg;
const frames = Math.round(+secArg * fps);

(async () => {
  const browser = await chromium.launch({
    executablePath: chrome || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  });
  const ctx = await browser.newContext({
    viewport: { width: 1080, height: 900 },
    deviceScaleFactor: 1,
    colorScheme: scheme,
    reducedMotion: "no-preference",
  });
  await ctx.addInitScript(() => {
    let s = 42;
    Math.random = () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296);
    let t = 0;
    let q = [];
    window.requestAnimationFrame = (cb) => {
      q.push(cb);
      return q.length;
    };
    window.__tick = (ms) => {
      t += ms;
      const run = q;
      q = [];
      run.forEach((cb) => cb(t));
    };
  });
  const page = await ctx.newPage();
  await page.goto("file://" + path.resolve(page_));
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(500);
  const el = await page.$(".plan-svg");
  await el.scrollIntoViewIfNeeded();
  await page.evaluate(() => window.__tick(0));
  fs.mkdirSync(outDir, { recursive: true });
  for (let i = 0; i < frames; i++) {
    await page.evaluate((ms) => window.__tick(ms), 1000 / fps);
    await el.screenshot({ path: path.join(outDir, String(i).padStart(4, "0") + ".png") });
  }
  await browser.close();
})();
