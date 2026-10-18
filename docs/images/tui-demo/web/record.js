// Records a tour of `hall web` over the fictional repos: `node record.js
// <url> <frames-dir> <hall-demo> <root>`, with the page served from the
// demo environment. It starts `hall-demo animate <root>` once the page is
// up, so the items move on cue. See ../record-web.sh.
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const puppeteer = require('puppeteer-core');

const [url, out, demo, root] = process.argv.slice(2);
const CHROME = process.env.CHROME
  || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const W = 1440;
const H = 860;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A drawn pointer: headless screenshots have none.
const CURSOR = `
  #demo-cursor { position: fixed; z-index: 99999; left: 0; top: 0; width: 22px; height: 22px;
    pointer-events: none; transform: translate(${W * 0.55}px, ${H * 0.45}px);
    transition: transform 650ms cubic-bezier(.4,0,.2,1); }
  #demo-cursor svg { filter: drop-shadow(0 1px 1.5px rgba(0,0,0,.35)); }
  #demo-cursor .ring { position: absolute; left: -11px; top: -11px; width: 22px; height: 22px;
    border-radius: 50%; border: 2px solid #9a6b12; opacity: 0; }
  #demo-cursor.click .ring { animation: ring 420ms ease-out; }
  @keyframes ring { from { transform: scale(.3); opacity: .9; } to { transform: scale(1.6); opacity: 0; } }`;

async function main() {
  fs.mkdirSync(out, { recursive: true });
  const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--hide-scrollbars'] });
  const page = await browser.newPage();
  await page.setViewport({ width: W, height: H, deviceScaleFactor: 1 });
  await page.emulateMediaFeatures([{ name: 'prefers-color-scheme', value: 'light' }]);
  await page.evaluateOnNewDocument(() => {
    localStorage.setItem('hall-theme', 'light');
    localStorage.setItem('hall-refresh', '2');
  });
  // The daemon's part: H-0012 goes to checks at 4s, H-0005 is approved at
  // 7s and shipped at 10s, H-0007 asks a question at 22s.
  const player = spawn(demo, ['animate', root], { stdio: 'ignore' });
  await page.goto(url, { waitUntil: 'networkidle0' });
  await page.waitForFunction(() => document.body.textContent.includes('Daemon running'), { timeout: 10000 });
  await page.addStyleTag({ content: CURSOR });
  await page.evaluate(() => {
    const c = document.createElement('div');
    c.id = 'demo-cursor';
    c.innerHTML = '<span class="ring"></span><svg width="22" height="22" viewBox="0 0 22 22"><path d="M3 2 L3 18 L7.5 14 L10.5 20.5 L13 19.4 L10.1 13 L16 13 Z" fill="#1b1c1d" stroke="#fff" stroke-width="1.4" stroke-linejoin="round"/></svg>';
    document.body.append(c);
  });
  await sleep(300);

  // Frames as fast as they come, each with its time; at the end they are
  // resampled to a steady rate.
  let recording = true;
  const times = [];
  const t0 = Date.now();
  const grab = (async () => {
    let n = 0;
    while (recording) {
      const buf = await page.screenshot({ type: 'png' });
      const file = `f${String(n++).padStart(5, '0')}.png`;
      fs.writeFileSync(path.join(out, file), buf);
      times.push([Date.now() - t0, file]);
    }
  })();

  // Moves the pointer to the first `sel` that has `text` (waiting for it to
  // appear, and scrolling it into view), then clicks it.
  // The page redraws on every refresh, so the element is found again for
  // its position once the scroll settles.
  const point = async (sel, { text = '', click = true } = {}) => {
    const find = (scroll) => page.waitForFunction((s, t, scroll) => {
      const e = [...document.querySelectorAll(s)].find((x) => x.textContent.includes(t));
      if (!e) return null;
      const r = e.getBoundingClientRect();
      if (scroll && (r.top < 90 || r.bottom > innerHeight - 60)) e.scrollIntoView({ block: 'center', behavior: 'smooth' });
      return scroll || { x: r.x, y: r.y, width: r.width, height: r.height };
    }, { timeout: 10000 }, sel, text, scroll);
    await find(true);
    await sleep(500);
    const box = await (await find(false)).jsonValue();
    const x = box.x + Math.min(box.width / 2, 60);
    const y = box.y + box.height / 2;
    await page.evaluate((x, y) => {
      document.getElementById('demo-cursor').style.transform = `translate(${x}px, ${y}px)`;
    }, x, y);
    await sleep(750);
    if (!click) return;
    await page.evaluate(() => {
      const c = document.getElementById('demo-cursor');
      c.classList.remove('click');
      void c.offsetWidth;
      c.classList.add('click');
    });
    await page.mouse.click(x, y);
  };
  const wheel = async (dy, steps = 6) => {
    for (let k = 0; k < steps; k++) {
      await page.mouse.wheel({ deltaY: dy / steps });
      await sleep(90);
    }
  };

  // The floor, live: H-0012 moves from the Workshop to the Assay Office.
  await sleep(1200);
  await point('.icard', { text: 'H-0012', click: false });
  await sleep(4000);
  // An item that's approved and shipped while you watch.
  await point('.icard', { text: 'H-0005' });
  await sleep(6500);
  await point('nav[aria-label="Item views"] a', { text: 'Findings' });
  await sleep(3000);
  await point('nav[aria-label="Item views"] a', { text: 'Diff' });
  await sleep(2800);
  // Your office: an agent's question, answered from the page.
  await point('header a, nav a', { text: 'Your office' });
  await sleep(2200);
  await point('a.card.entry', { text: 'per API key' });
  await sleep(2200);
  await point('[data-answer="key"]');
  await sleep(900);
  await page.evaluate(() => {
    document.getElementById('demo-cursor').style.transform = `translate(${innerWidth * 0.62}px, ${innerHeight * 0.16}px)`;
  });
  await sleep(2400);
  // What it costs.
  await point('header a, nav a', { text: 'Usage' });
  await sleep(2500);
  await page.mouse.move(W / 2, H / 2);
  await wheel(420);
  await sleep(2500);
  await wheel(-420);
  await point('header a, nav a', { text: 'Floor' });
  await sleep(3500);

  recording = false;
  await grab;
  player.kill();
  await browser.close();

  // A steady 10 fps: each tick shows the latest frame taken by then.
  const steady = path.join(out, 'steady');
  fs.mkdirSync(steady, { recursive: true });
  const end = times[times.length - 1][0];
  for (let t = 0, k = 0, n = 0; t <= end; t += 100, n++) {
    while (k + 1 < times.length && times[k + 1][0] <= t) k++;
    fs.copyFileSync(path.join(out, times[k][1]), path.join(steady, `s${String(n).padStart(5, '0')}.png`));
  }
}

main().catch((e) => { console.error(e); process.exit(1); });
