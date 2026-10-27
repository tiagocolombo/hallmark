// hall web (ADR 0052). Every view runs hall's own commands through
// POST /api/run and draws their JSON replies; every button runs a command
// a reply offered (`actions`, `next`, `answer_with`), so what each state
// allows stays decided by hall, not by this page.
'use strict';

const $ = (sel, el = document) => el.querySelector(sel);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
}[c]));

const S = {
  token: null,
  repos: [],
  repo: null,
  seq: 0,
  running: 0,
  lastDraw: 0,
  update: null,
  data: null,
  daemon: null,
  costs: new Map(),
  ticks: 0,
  lastTick: 0,
  refresh: 5,
  theme: 'system',
  noRail: false,
  noFeed: false,
  feedShown: 30,
  floorFilter: 'all',
  groupBy: 'room',
  sortBy: 'status',
  q: '',
  hist: { rejected: false, lens: '', since: '30d' },
  libq: '',
  usage: { by: 'room', since: '14d' },
  cfg: { group: '', user: false },
  logKind: 'all',
  newMode: 'request',
};

// The commands the server runs for the page (cli::allowed_on_web decides;
// this only picks which offered commands get a button).
const WEB = new Set(['status', 'log', 'inbox', 'ready', 'pull-back', 'drop', 'retry', 'continue',
  'answer', 'usage', 'reindex', 'config', 'findings', 'merge', 'history', 'stats', 'queue',
  'export', 'budget', 'notify', 'library', 'extensions', 'hud', 'doctor', 'new', 'adopt',
  'trace', 'daemon', 'update', 'gazette']);
// Readers: offered as links to the view that shows them, not as buttons.
const READERS = new Set(['status', 'log', 'findings', 'trace', 'history', 'stats', 'queue',
  'usage', 'hud', 'inbox', 'watch']);

// ---------------------------------------------------------------- plumbing

function readToken() {
  const m = location.hash.match(/token=([0-9a-f]+)/);
  if (m) {
    S.token = m[1];
    try { sessionStorage.setItem('hall-token', S.token); } catch (_) { /* private mode */ }
    history.replaceState(null, '', location.pathname + '#/floor');
    return;
  }
  try { S.token = sessionStorage.getItem('hall-token'); } catch (_) { S.token = null; }
}

async function api(path, body) {
  let r;
  try {
    r = await fetch(path, {
      method: body ? 'POST' : 'GET',
      headers: Object.assign({ 'X-Hall-Token': S.token || '' },
        body ? { 'Content-Type': 'application/json' } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch (e) {
    return { error: { code: 'INTERNAL', message: 'hall web is not answering: is it still running?', next: ['hall web'] } };
  }
  let j;
  try { j = await r.json(); } catch (_) { j = { error: { code: 'INTERNAL', message: `HTTP ${r.status}` } }; }
  if (r.status === 401) { S.locked = true; locked(); }
  return j;
}

// Runs `hall <args>` in the selected repo: {exit, reply, lines}.
async function hall(args, repo) {
  const j = await api('/api/run', { repo: repo || S.repo, args });
  if (j && j.error) return { exit: 2, reply: j, lines: [] };
  return j;
}

// `hall a "b c"` → ['a', 'b c'], as a shell reads it.
function split(cmd) {
  const out = [];
  let cur = '', q = null, any = false;
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (q) {
      if (c === q) q = null;
      else if (c === '\\' && q === '"' && i + 1 < cmd.length) cur += cmd[++i];
      else cur += c;
    } else if (c === '"' || c === "'") { q = c; any = true; }
    else if (/\s/.test(c)) { if (cur || any) { out.push(cur); cur = ''; any = false; } }
    else cur += c;
  }
  if (cur || any) out.push(cur);
  return out;
}

const quote = (a) => (/^[\w@%+=:,./-]+$/.test(a) ? a : `"${a.replace(/(["\\$`])/g, '\\$1')}"`);
const command = (args) => 'hall ' + args.map(quote).join(' ');

function runnable(cmd) {
  const a = split(cmd);
  if (a[0] !== 'hall' || !WEB.has(a[1])) return false;
  if (a.includes('--follow') || (a[1] === 'new' && a.includes('--open'))) return false;
  return true;
}

function toast(msg, err) {
  const t = document.createElement('div');
  t.className = 'toast' + (err ? ' err' : '');
  t.textContent = msg;
  $('#toasts').append(t);
  setTimeout(() => t.remove(), err ? 9000 : 5000);
}

const errorOf = (r) => (r && r.reply && r.reply.error) || null;

// The words a reply gives for what happened.
function said(reply) {
  if (!reply) return 'done';
  for (const k of ['message', 'note', 'summary', 'doing']) if (typeof reply[k] === 'string') return reply[k];
  if (reply.now && reply.now.doing) return reply.now.doing;
  if (reply.id && reply.state) return `${reply.id} is ${String(reply.state).replace(/_/g, ' ')}`;
  return 'done';
}

// Runs an offered command. Placeholders such as "<why>" are asked for first.
async function run(cmd, opts = {}) {
  // `opts.args` are used as they are: typed text is never a template.
  let args = opts.args || split(cmd).slice(1);
  const holes = opts.args ? [] : [...new Set(args.filter((a) => /<[^>]+>/.test(a)))];
  if (holes.length || opts.confirm) {
    const filled = await ask(opts.title || cmd, holes, opts.confirm);
    if (!filled) return null;
    args = args.map((a) => (a in filled ? filled[a] : a));
  }
  S.running++;
  const btn = opts.button;
  if (btn) { btn.disabled = true; btn.dataset.label = btn.innerHTML; btn.innerHTML = '<span class="spin"></span>' + btn.innerHTML; }
  const r = await hall(args);
  S.running--;
  if (btn && btn.isConnected) { btn.disabled = false; btn.innerHTML = btn.dataset.label; }
  const e = errorOf(r);
  if (e) toast(`${e.code}: ${e.message}`, true);
  else if (!opts.quiet) toast(said(r.reply));
  if (!opts.noRefresh && !(e && opts.keepOnError)) { draw(); sidebar(); }
  return r;
}

// A dialog asking for each placeholder; resolves to {placeholder: text}.
function ask(title, holes, confirm) {
  const d = $('#dialog');
  d.innerHTML = `<form method="dialog">
    <h2>${esc(title)}</h2>
    ${confirm ? `<p style="margin:0">${esc(confirm)}</p>` : ''}
    ${holes.map((h, i) => `<label class="field">${esc(h.replace(/[<>]/g, ''))}
      <textarea rows="2" name="h${i}" required></textarea></label>`).join('')}
    <div class="row"><button class="btn primary" value="ok">Run</button>
    <button class="btn" value="cancel" formnovalidate>Cancel</button></div></form>`;
  return new Promise((resolve) => {
    d.onclose = () => {
      if (d.returnValue !== 'ok') return resolve(null);
      const out = {};
      holes.forEach((h, i) => { out[h] = d.querySelector(`[name=h${i}]`).value.trim(); });
      resolve(out);
    };
    d.returnValue = '';
    d.showModal();
    const first = d.querySelector('textarea');
    if (first) first.focus();
  });
}

// ------------------------------------------------------------- formatting

function tok(n) {
  n = Number(n || 0);
  if (n >= 1e9) return (n / 1e9).toFixed(1) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1) + 'M';
  if (n >= 1e3) return Math.round(n / 1e3) + 'k';
  return String(n);
}
const usd = (n) => '$' + Number(n || 0).toFixed(2);
function ago(iso) {
  if (!iso) return '';
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 60) return 'just now';
  if (s < 3600) return Math.round(s / 60) + 'm ago';
  if (s < 86400) return Math.round(s / 3600) + 'h ago';
  return Math.round(s / 86400) + 'd ago';
}
const clock = (iso) => (iso ? new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '');
const day = (iso) => (iso ? new Date(iso).toLocaleDateString([], { month: 'short', day: 'numeric' }) : '');
const short = (sha) => (sha ? String(sha).slice(0, 7) : '');
const label = (s) => String(s || '').replace(/_/g, ' ');

const STATE_TONE = {
  development: 'info', awaiting_you: 'you', checks: 'principal', review: 'principal',
  fixing: 'you', escalated: 'stop', hallmarked: 'ok', closed: 'ok', dropped: '',
};
const pill = (state) => `<span class="pill ${STATE_TONE[state] || ''}">${esc(label(state))}</span>`;

function roleColor(actor) {
  const a = String(actor || '');
  if (/^(dev|hook|session|agent)/.test(a)) return 'var(--dev)';
  if (/(assayer|review|principal)/.test(a)) return 'var(--principal)';
  if (/fixer/.test(a)) return 'var(--fixer)';
  if (/^you|human/.test(a)) return 'var(--you)';
  if (/(ship|deliver|merger|sweeper)/.test(a)) return 'var(--ship)';
  return 'var(--clerk)';
}

// Buttons for offered commands; readers and commands this page can't run
// are shown as commands to copy.
function actions(list, opts = {}) {
  const seen = new Set();
  const out = [];
  for (const a of list || []) {
    const cmd = typeof a === 'string' ? a : a.command;
    const does = typeof a === 'string' ? '' : a.does;
    if (!cmd || seen.has(cmd)) continue;
    seen.add(cmd);
    const verb = split(cmd)[1];
    if (runnable(cmd) && !READERS.has(verb)) {
      const danger = verb === 'drop' ? ' danger' : '';
      out.push(`<button type="button" class="btn${danger}${opts.sm ? ' sm' : ''}" data-run="${esc(cmd)}" title="${esc(cmd)}">${esc(does ? cap(does) : verbLabel(cmd))}</button>`);
    } else if (opts.copy !== false && !READERS.has(verb)) {
      out.push(copyable(cmd, does));
    }
  }
  return out.join('');
}
const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);
function verbLabel(cmd) {
  const a = split(cmd).slice(1).filter((x) => !/^H-\d+$/.test(x) && !/^Q-\d+$/.test(x));
  return cap(a.filter((x) => !x.startsWith('-')).slice(0, 2).join(' ') || a.join(' '));
}
const copyable = (cmd, does) => `<div class="cmd" style="flex:1 1 100%"><code>${esc(cmd)}</code>${does ? `<span class="small muted">${esc(does)}</span>` : ''}<button type="button" class="btn sm" data-copy="${esc(cmd)}">Copy</button></div>`;

function failure(r, what) {
  const e = errorOf(r);
  if (!e) return '';
  return `<div class="card" style="border-color:var(--stop)"><b>${esc(what)}: ${esc(e.code)}</b><div>${esc(e.message)}</div>
    ${e.next && e.next.length ? `<div class="row">${e.next.map((n) => copyable(n)).join('')}</div>` : ''}</div>`;
}

const head = (eyebrow, title, right = '') => `<header class="head"><div>
  <div class="eyebrow">${esc(eyebrow)}</div><h1>${esc(title)}</h1></div>${right}</header>`;

// ------------------------------------------------------------------ views

// The rooms an item walks through, in order, with the building's colours.
const ROOMS = [
  ['workshop', 'Workshop', 'var(--dev)', ['development']],
  ['office', 'Your office', 'var(--you)', ['awaiting_you', 'escalated']],
  ['assay', 'Assay office', 'var(--principal)', ['checks', 'review']],
  ['repair', 'Repair shop', 'var(--fixer)', ['fixing']],
  ['dispatch', 'Dispatch & delivery', 'var(--ship)', ['hallmarked', 'closed']],
  ['records', 'Records', 'var(--clerk)', ['dropped']],
];
const roomOf = (state) => ROOMS.find((r) => r[3].includes(state)) || ROOMS[5];
// Where an item is on its way, from 1 (development) to 7 (merged).
const STAGES = ['development', 'checks', 'review', 'fixing', 'hallmarked', 'closed'];
function stageOf(i) {
  const k = { awaiting_you: 0, escalated: 3, dropped: 0 }[i.state] ?? STAGES.indexOf(i.state);
  return Math.max(k, 0) + 1 + (i.delivered === 'merged' ? 1 : 0);
}
const SEVERITY = [['blocker', 'var(--stop)'], ['major', 'var(--fixer)'], ['minor', 'var(--warn)'], ['nit', 'var(--clerk)']];

// What the floor shows for an item right now: [tone, glyph, words].
function band(i) {
  const d = S.data;
  const loop = d.loop.get(i.id);
  if (i.state === 'escalated') return ['stop', '⚠', 'Escalated: it needs you'];
  if (i.state === 'awaiting_you' || d.needs.has(i.id)) return ['you', '⏸', loop ? loop.now : 'Waiting for you'];
  if (loop) return loop.stuck ? ['you', '⚠', 'Stuck: ' + loop.now] : ['run', '▶', loop.now];
  if (i.state === 'development') return ['dev', '✎', 'In development'];
  if (i.state === 'hallmarked') return ['ok', '✓', 'Hallmarked: on its way to a PR'];
  if (i.state === 'closed') return ['ok', '✓', i.delivered === 'merged' ? 'Merged' : 'Shipped: the Delivery office watches its PR'];
  return ['', '·', label(i.state)];
}

function spark(values, w = 160, h = 26) {
  if (values.length < 2) return '';
  const max = Math.max(...values, 1);
  const pts = values.map((v, k) => `${((k * w) / (values.length - 1)).toFixed(1)},${(h - 2 - (v / max) * (h - 4)).toFixed(1)}`).join(' ');
  return `<svg class="spark" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" aria-hidden="true"><polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" vector-effect="non-scaling-stroke"/></svg>`;
}

async function floor() {
  await refreshShell();
  const d = S.data;
  if (d.error) return failure(d.error, 'hall status');
  const [days, costs] = await Promise.all([
    hall(['usage', '--by', 'day', '--since', '14d']), hall(['usage', '--by', 'item'])]);
  const rows = ((days.reply && days.reply.rows) || []).slice().sort((a, b) => String(a.key).localeCompare(String(b.key)));
  S.costs = new Map(((costs.reply && costs.reply.rows) || []).map((r) => [r.key, r.cost_usd]));
  const items = d.items;
  const today = d.hud.today || {};
  const rejects = items.reduce((n, i) => n + (i.rejects || 0), 0);
  const inDev = items.filter((i) => i.state === 'development').length;
  const shipped = items.filter((i) => i.state === 'closed' || i.state === 'hallmarked').length;
  const waiting = d.hud.inbox || 0;
  return `<section class="kpis" aria-label="Summary">
    <div class="kpi"><span class="l">Open items</span><span class="n">${items.length}</span><span class="s">${inDev} in development · ${shipped} shipped, not merged</span></div>
    <div class="kpi"><span class="l">In the review loop</span><span class="n">${d.loop.size}</span><span class="s">${esc(d.hud.daemon && d.hud.daemon.current ? d.hud.daemon.current : 'nothing running')}</span></div>
    <a class="kpi${waiting ? ' warn' : ''}" href="#/office"><span class="l">Waiting for you</span><span class="n">${waiting}</span><span class="s">${waiting ? 'questions, escalations, holds' : 'nothing'}</span></a>
    <div class="kpi"><span class="l">Rejections</span><span class="n">${rejects}</span><span class="s">across open items · the 3rd escalates</span></div>
    <a class="kpi" href="#/usage"><span class="l">Est. cost today</span><span class="n">${usd(today.cost_usd)}</span>${spark(rows.map((r) => r.cost_usd || 0))}<span class="s">the loop ${usd(today.loop_usd)} · 14 days</span></a>
    <a class="kpi" href="#/usage"><span class="l">Tokens today</span><span class="n">${tok(today.tokens)}</span>${spark(rows.map((r) => r.tokens || 0))}<span class="s">the loop ${tok(today.loop_tokens)}${today.daily_cap ? ' of ' + tok(today.daily_cap) : ''}</span></a>
  </section>
  <div id="floor-body">${floorBody()}</div>
  <footer class="legend">
    <span><i class="sq" style="--c:var(--stop)"></i>blocker</span><span><i class="sq" style="--c:var(--fixer)"></i>major</span>
    <span><i class="sq" style="--c:var(--warn)"></i>minor</span><span><i class="sq" style="--c:var(--clerk)"></i>nit</span>
    <span>open findings</span><span class="grow"></span><span>Square colour is the room:</span>
    ${ROOMS.slice(0, 5).map(([, name, c]) => `<span class="chip sq-chip"><i class="sq" style="--c:${c}"></i>${esc(name)}</span>`).join('')}
  </footer>`;
}

const FILTERS = [['all', 'All'], ['loop', 'In the loop'], ['you', 'Waiting for you'], ['dev', 'In development'], ['shipped', 'Shipped']];

function floorBody() {
  const seg = (key, vals) => `<div class="seg" role="group">${vals.map(([v, l]) =>
    `<button type="button" class="${S[key] === v ? 'on' : ''}" data-set="${key}" data-val="${v}" data-local>${l}</button>`).join('')}</div>`;
  return `<div class="filters">
      <span>Group by</span>${seg('groupBy', [['room', 'Room'], ['state', 'State'], ['none', 'None']])}
      <span>Sort by</span><select data-bind="sortBy" data-local aria-label="Sort by">${[['status', 'Status'], ['newest', 'Newest'], ['oldest', 'Oldest'], ['round', 'Round'], ['cost', 'Cost']].map(([v, l]) =>
        `<option value="${v}" ${S.sortBy === v ? 'selected' : ''}>${l}</option>`).join('')}</select>
    </div>
    <div class="filters">${seg('floorFilter', FILTERS)}
      <input type="search" id="q" value="${esc(S.q)}" placeholder="Filter by ID, title or branch" aria-label="Filter items">
      <span class="grow"></span><span id="shown"></span>
    </div>
    <div id="cards">${cardsHtml()}</div>`;
}

function shownItems() {
  const d = S.data;
  const q = S.q.toLowerCase();
  const keep = {
    all: () => true,
    loop: (i) => d.loop.has(i.id),
    you: (i) => d.needs.has(i.id) || i.state === 'awaiting_you' || i.state === 'escalated',
    dev: (i) => i.state === 'development',
    shipped: (i) => i.state === 'hallmarked' || i.state === 'closed',
  }[S.floorFilter] || (() => true);
  const rank = (i) => ROOMS.indexOf(roomOf(i.state));
  const sorts = {
    status: (a, b) => rank(a) - rank(b) || String(a.id).localeCompare(String(b.id)),
    newest: (a, b) => String(b.created_at).localeCompare(String(a.created_at)),
    oldest: (a, b) => String(a.created_at).localeCompare(String(b.created_at)),
    round: (a, b) => (b.round || 0) - (a.round || 0),
    cost: (a, b) => (S.costs.get(b.id) || 0) - (S.costs.get(a.id) || 0),
  };
  return d.items
    .filter(keep)
    .filter((i) => !q || [i.id, i.title, i.branch].some((s) => String(s || '').toLowerCase().includes(q)))
    .sort(sorts[S.sortBy] || sorts.status);
}

function cardsHtml() {
  const items = shownItems();
  const shown = document.getElementById('shown');
  const total = `${items.length} shown · ${S.data.items.length} open`;
  if (shown) shown.textContent = total;
  else setTimeout(() => { const s = document.getElementById('shown'); if (s) s.textContent = total; });
  if (!items.length) return `<div class="empty">${S.data.items.length ? 'No item matches.' : 'No open items. Start one with New item, or `hall new` in a session.'}</div>`;
  if (S.groupBy === 'none') return `<div class="cards">${items.map(icard).join('')}</div>`;
  const groups = new Map();
  for (const i of items) {
    const k = S.groupBy === 'room' ? roomOf(i.state)[1] : label(i.state);
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(i);
  }
  return [...groups].map(([k, list]) => {
    const c = S.groupBy === 'room' ? roomOf(list[0].state)[2] : roomOf(list[0].state)[2];
    return `<div class="group-h"><i class="sq" style="--c:${c}"></i><b>${esc(k)}</b><span>${list.length}</span></div>
      <div class="cards">${list.map(icard).join('')}</div>`;
  }).join('');
}

function icard(i) {
  const d = S.data;
  const loop = d.loop.get(i.id);
  const [tone, glyph, words] = band(i);
  const room = roomOf(i.state);
  const stage = stageOf(i);
  const open = (loop && loop.open) || {};
  const squares = SEVERITY.flatMap(([k, c]) => Array.from({ length: open[k] || 0 }, () => `<i style="--c:${c}" title="${k}"></i>`));
  const steps = Array.from({ length: 7 }, (_, k) => `<i style="--c:${k < stage ? room[2] : 'var(--line)'}"></i>`).join('');
  const nOpen = SEVERITY.reduce((n, [k]) => n + (open[k] || 0), 0);
  const pr = i.pr_url ? `<span>PR #${esc(i.pr_url.split('/').pop())}</span>` : '';
  const cost = S.costs.get(i.id);
  const rejects = i.rejects || 0;
  const cardTone = tone === 'stop' ? ' stop' : tone === 'you' ? ' you' : '';
  return `<a class="icard${cardTone}" href="#/item/${esc(i.id)}">
    <div class="ic-sec">
      <div class="ic-head"><span class="caret">▶</span><span class="id">${esc(i.id)}</span><span class="ttl">${esc(i.title)}</span>
        <span class="chip sq-chip"><i class="sq" style="--c:${room[2]}"></i>${esc(label(i.state))}</span>
        <span class="meter${rejects >= 3 ? ' stop' : ''}" title="${rejects} rejection${rejects === 1 ? '' : 's'}; the 3rd escalates">${[0, 1, 2].map((k) => `<i class="${k < rejects ? 'on' : ''}"></i>`).join('')}</span></div>
      <div class="ic-meta"><span class="br">⎇ ${esc(i.branch)}</span><span class="tag">→ ${esc(i.base)}</span>${i.sha ? `<span>${esc(short(i.sha))}</span>` : ''}${pr}</div>
    </div>
    <div class="ic-sec ic-plan">
      <div class="doing"><span>${esc(loop ? loop.now : i.summary || room[1])}</span><span class="mono small muted">${stage}/7</span></div>
      <div class="squares" aria-label="Stage ${stage} of 7">${steps}</div>
      <div class="row small muted">${nOpen ? `<span class="squares">${squares.join('')}</span><span>${SEVERITY.filter(([k]) => open[k]).map(([k]) => `${open[k]} ${k}`).join(' · ')} open</span>` : '<span>No open findings</span>'}</div>
    </div>
    <div class="ic-stats">
      <div><span class="l">Round</span><span class="v">${esc(i.round || 0)}</span></div>
      <div><span class="l">Rejects${rejects ? ` <span class="chip sq-chip ${rejects >= 3 ? 'stop' : 'warn'}" style="min-height:16px;padding:0 4px">⚠ ${rejects}×</span>` : ''}</span><span class="v">${rejects}</span></div>
      <div><span class="l">In state</span><span class="v">${esc(ago(i.state_since).replace(' ago', ''))}</span></div>
      <div><span class="l">Cost</span><span class="v">${cost != null ? usd(cost) : '—'}</span></div>
    </div>
    <div class="band ${tone}"><span>${glyph}</span><span>${esc(words)}</span></div>
  </a>`;
}

const STEPS = [
  ['Development', ['development', 'awaiting_you']], ['Checks', ['checks']], ['Review', ['review']],
  ['Fixing', ['fixing', 'escalated']], ['Hallmarked', ['hallmarked']], ['Shipped', ['closed']],
];

async function item(id, tab = 'journal', arg) {
  const st = await hall(['status', id]);
  if (errorOf(st)) return head('Item', id) + failure(st, `hall status ${id}`);
  const i = st.reply;
  const at = STEPS.findIndex(([, s]) => s.includes(i.state));
  const steps = STEPS.map(([name], n) => `<li class="${n < at ? 'done' : n === at ? 'here' : ''}">${esc(name)}${n === at && i.round ? ' · round ' + i.round : ''}</li>`).join('')
    + `<li class="${i.delivered === 'merged' ? 'here' : ''}">${i.delivered ? esc(cap(i.delivered)) : 'Merged'}</li>`;
  const now = i.now || {};
  const live = now.status === 'running';
  const terminal = i.state === 'closed' || i.state === 'dropped';
  const extra = [];
  if (!terminal) extra.push({ command: `hall drop ${id} --reason "<why>"`, does: 'drop it' });
  extra.push({ command: `hall export ${id}`, does: 'export its record' });
  const tabs = ['journal', 'findings', 'diff', 'runs', 'usage', 'gazette'].map((t) =>
    `<a href="#/item/${esc(id)}/${t}" class="${t === tab ? 'on' : ''}">${cap(t)}</a>`).join('');
  const body = await ({ journal, findings, diff, runs, itemUsage, gazette: itemGazette }[tab === 'usage' ? 'itemUsage' : tab] || journal)(id, arg, i);
  return `<div class="small muted"><a href="#/floor">The floor</a> / <span class="mono">${esc(id)}</span></div>
  <header class="head"><div style="display:flex;flex-direction:column;gap:8px">
    <div class="row"><span class="mono muted">${esc(id)}</span>${pill(i.state)}<span class="small muted">${esc(i.room)} · since ${esc(ago(i.state_since))}</span></div>
    <h1 style="font-size:20px">${esc(i.title)}</h1>
    <div class="row mono small muted" style="gap:6px 18px"><span>${esc(i.branch)} → ${esc(i.base)}</span>
      ${i.sha ? `<span>${esc(short(i.sha))}</span>` : ''}<span>${esc(i.worktree)}</span>
      ${i.rejects ? `<span>${i.rejects} rejection${i.rejects > 1 ? 's' : ''}</span>` : ''}
      ${i.pr_url ? `<a href="${esc(i.pr_url)}" target="_blank" rel="noopener noreferrer">${esc(i.pr_url.replace(/^https:\/\/github.com\//, ''))}</a>` : ''}</div>
  </div></header>
  <ol class="steps" aria-label="Where it is">${steps}</ol>
  ${now.doing ? `<section class="card nowbox${live ? ' live' : ''}" aria-label="Now">
    <span class="dot ${live ? 'ok' : now.status === 'waiting_for_you' ? 'warn' : ''}"></span>
    <div class="grow" style="flex:1 1 300px"><b>${esc(now.doing)}</b>
      <div class="small muted">${esc([now.elapsed, ...(now.detail || [])].filter(Boolean).join(' · '))}</div></div>
    ${live && tab !== 'runs' ? `<a class="btn primary" href="#/item/${esc(id)}/runs">Watch live</a>` : ''}
    <div class="row" style="flex:1 1 100%">${actions([...(now.actions || []), ...extra])}</div>
    ${now.log && now.log.length ? `<pre class="term" style="margin:0;flex:1 1 100%;max-height:220px">${esc(now.log.join('\n'))}</pre>` : ''}
  </section>` : `<div class="row">${actions(extra)}</div>`}
  ${i.summary ? `<p style="margin:0">${esc(i.summary)}</p>` : ''}
  <nav class="tabs" aria-label="Item views">${tabs}</nav>${body}`;
}

function eventText(e) {
  for (const k of ['text', 'subject', 'summary', 'question', 'answer', 'reason', 'note', 'message', 'doing']) {
    if (typeof e[k] === 'string' && e[k]) return e[k];
  }
  if (e.event === 'usage') return `${tok((e.input_tokens || 0) + (e.output_tokens || 0) + (e.cache_read_tokens || 0) + (e.cache_write_tokens || 0))} tokens · ${usd(e.cost_usd)} · ${e.model || ''}`;
  if (e.fixes) return 'fixes ' + e.fixes.join(' ');
  if (e.verdict) return String(e.verdict.decision || e.verdict.outcome || e.verdict);
  if (e.decision) return String(e.decision);
  if (e.results) return (e.results || []).map((g) => `${g.gate || g.name}: ${g.ok === false || g.passed === false ? 'failed' : 'passed'}`).join(' · ');
  if (e.sha) return short(e.sha);
  return '';
}

async function journal(id) {
  const args = ['log', id, '--limit', '300'];
  if (S.logKind !== 'all') args.push('--kind', S.logKind);
  const r = await hall(args);
  if (errorOf(r)) return failure(r, 'hall log');
  const ev = (r.reply.events || []).slice().reverse();
  const opts = ['all', 'prompt', 'commit', 'ready', 'checks', 'review', 'fix', 'usage'].map((k) =>
    `<option ${k === S.logKind ? 'selected' : ''}>${k}</option>`).join('');
  return `<div class="row"><label class="row small muted">Kinds <select data-bind="logKind">${opts}</select></label>
    <span class="small muted">${ev.length} of ${r.reply.total ?? ev.length} events, newest first</span></div>
  <ol class="timeline">${ev.map((e) => {
    const text = eventText(e);
    return `<li style="--c:${roleColor(e.actor)}"><div class="meta"><span class="mono">${esc(day(e.t))} ${esc(clock(e.t))}</span>
      <b>${esc(label(e.event))}</b><span>${esc(e.actor || '')}</span>${e.sha && text !== short(e.sha) ? `<span class="mono">${esc(short(e.sha))}</span>` : ''}</div>
      ${text ? `<div class="body">${esc(text)}</div>` : ''}</li>`;
  }).join('') || '<li>No events.</li>'}</ol>`;
}

// The item's Gazette (`hall gazette <id> --html`), framed: no scripts run in
// it, its links open a new tab, and it takes the page's theme.
async function itemGazette(id) {
  const r = await hall(['gazette', id, '--html']);
  const e = errorOf(r);
  if (e && e.code === 'WRONG_STATE') {
    return '<div class="empty">The librarian prints the Gazette when the item finishes: closed, escalated, or dropped.</div>';
  }
  if (e) return failure(r, 'hall gazette');
  const theme = S.theme === 'light' || S.theme === 'dark' ? ` data-theme="${S.theme}"` : '';
  // Attached files sit next to the page on disk, not under the app's URL:
  // show each as its name and where it is, not a link that would 404.
  const dir = String(r.reply.path || '').replace(/[\\/][^\\/]*$/, '');
  const doc = String(r.reply.html || '').replace('<html lang="en">', `<html lang="en"${theme}>`)
    .replace(/<a href="(attachments\/[^"]*)">([^<]*)<\/a>/g,
      (_, p, name) => `${name} <span class="mono">(${dir}/${p})</span>`)
    .replace('<head>', '<head><base target="_blank">');
  return `${r.reply.prose ? '' : '<div class="empty">In the item\'s own words: the librarian hasn\'t written it up yet.</div>'}
  <iframe class="gazette" title="The Gazette for ${esc(id)}" sandbox="allow-popups allow-popups-to-escape-sandbox" srcdoc="${esc(doc)}"></iframe>`;
}

const SEV = { blocker: 'stop', major: 'stop', minor: '', nit: '' };

async function findings(id) {
  const r = await hall(['findings', id, '--all']);
  if (errorOf(r)) return failure(r, 'hall findings');
  const f = r.reply.findings || [];
  const checks = r.reply.failing_checks || [];
  return `${checks.map((c) => `<div class="card" style="border-color:var(--stop)"><b>Failing check: ${esc(c.gate || c.name || '')}</b>
      <pre class="mono small" style="margin:0;white-space:pre-wrap">${esc(c.summary || c.log || JSON.stringify(c))}</pre></div>`).join('')}
    ${f.map((x) => `<div class="card"><div class="row small"><span class="pill ${SEV[x.severity] || ''}">${esc(x.severity)}</span>
      <span class="muted">${esc(x.lens)} · ${esc(x.id)} · round ${esc(x.round)}</span>
      <span class="mono muted">${esc(x.file)}${x.line ? ':' + esc(x.line) : ''}</span>
      <span class="grow"></span><span class="pill ${x.status === 'fixed' ? 'ok' : x.status === 'open' ? 'stop' : 'you'}">${esc(x.status)}</span></div>
      <b>${esc(x.title)}</b>
      ${x.scenario ? `<div class="muted" style="white-space:pre-wrap">${esc(x.scenario)}</div>` : ''}
      ${x.fix ? `<div><b class="small">Fix:</b> ${esc(x.fix)}</div>` : ''}
      ${x.dispute ? `<div><b class="small">Disputed:</b> ${esc(x.dispute)}</div>` : ''}</div>`).join('')
    || (checks.length ? '' : '<div class="empty">No findings.</div>')}`;
}

async function diff(id) {
  const q = new URLSearchParams({ repo: S.repo, id });
  const r = await api('/api/diff?' + q);
  if (r.error) return failure({ reply: r }, 'diff');
  const lines = (r.lines || []).map((l) => {
    const c = l.startsWith('+++') || l.startsWith('---') || l.startsWith('diff ') ? 'file'
      : l.startsWith('+') ? 'add' : l.startsWith('-') ? 'del' : l.startsWith('@@') ? 'hunk' : '';
    return `<span class="${c}">${esc(l)}</span>`;
  }).join('\n');
  return `<div class="small muted mono">git diff ${esc(r.range)}${r.more ? ` · ${r.more} more lines not shown` : ''}</div>
    <pre class="diff">${lines || '(no changes)'}</pre>`;
}

async function runs(id, n) {
  const list = await hall(['trace', id, '--list']);
  if (errorOf(list)) return failure(list, 'hall trace');
  const all = list.reply.runs || [];
  if (!all.length) return '<div class="empty">No headless runs yet: the reviewer, fixer and Front Desk runs show here.</div>';
  const pick = Number(n) || all[all.length - 1].n;
  const r = await hall(['trace', id, '--run', String(pick)]);
  const run = (r.reply && r.reply.run) || all.find((x) => x.n === pick) || {};
  const steps = r.lines || [];
  const state = (x) => (x.ended ? (x.failure ? `failed: ${x.failure}` : 'ended') : x.interrupted ? 'interrupted' : 'running');
  const side = all.slice().reverse().map((x) => `<a class="card${x.n === pick ? ' entry on' : ''}" href="#/item/${esc(id)}/runs/${x.n}" style="padding:10px 12px">
    <div class="row small"><span class="mono">#${x.n} ${esc(x.role)}</span><span class="grow"></span>
    <span class="${x.ended ? 'muted' : ''}" style="${x.ended ? '' : 'color:var(--principal)'}">${esc(state(x))}</span></div>
    <div class="small muted">${esc(x.model || '')} · ${esc(ago(x.started))}</div></a>`).join('');
  const body = steps.map((s) => {
    const k = s.kind === 'tool' ? (s.name || 'tool') : s.kind;
    const txt = s.kind === 'text' ? `<span class="say">${esc(s.text)}</span>` : esc(s.text);
    return `<div class="s${s.nested ? ' nested' : ''}"><span>${esc(clock(s.t))}</span><div><span class="${s.error ? 'e' : 'k'}">${esc(k)}</span>  ${txt}</div></div>`;
  }).join('');
  return `<div class="cols"><aside class="narrow" style="flex:1 1 220px" aria-label="Runs">${side}</aside>
    <section class="wide" aria-label="Steps"><div class="row"><h2 class="grow">Run ${run.n || pick} · ${esc(run.role || '')}</h2>
      ${run.ended && run.resume ? `<button type="button" class="btn sm" data-copy="${esc(run.resume)}">Copy ${esc(run.resume.includes('&&') ? 'cd … && claude --resume' : 'claude --resume')}</button>` : ''}</div>
      ${run.ended ? '' : '<div class="small muted">Following live: new steps show as they come.</div>'}
      <div class="term">${body || '<div style="color:var(--term-dim)">No steps yet.</div>'}</div></section></div>`;
}

async function itemUsage(id) {
  const r = await hall(['usage', '--by', 'role', '--item', id]);
  if (errorOf(r)) return failure(r, 'hall usage');
  return usageTable(r.reply, 'Role');
}

function usageTable(u, what) {
  const rows = u.rows || [];
  return `<div class="table"><table><thead><tr><th scope="col">${esc(what)}</th><th class="num" scope="col">Tokens</th>
    <th class="num" scope="col">Output</th><th class="num" scope="col">Runs</th><th class="num" scope="col">Cost</th></tr></thead><tbody>
    ${rows.map((x) => `<tr><td>${esc(x.key)}</td><td class="num">${tok(x.tokens)}</td><td class="num">${tok(x.output_tokens)}</td>
      <td class="num">${esc(x.records)}</td><td class="num">${usd(x.cost_usd)}</td></tr>`).join('')}
    ${u.total ? `<tr><td><b>Total</b></td><td class="num"><b>${tok(u.total.tokens)}</b></td><td class="num">${tok(u.total.output_tokens)}</td>
      <td class="num">${esc(u.total.records)}</td><td class="num"><b>${usd(u.total.cost_usd)}</b></td></tr>` : ''}
    </tbody></table></div>`;
}

async function office(tab = 'inbox', n) {
  const flag = { notices: ['--notices'], all: ['--all'] }[tab] || [];
  const r = await hall(['inbox', ...flag]);
  if (errorOf(r)) return head('Only what needs a person', 'Your office') + failure(r, 'hall inbox');
  const entries = r.reply.entries || [];
  const tabs = [['inbox', 'Inbox'], ['notices', 'Notices'], ['all', 'With dismissed']].map(([k, l]) =>
    `<a href="#/office/${k}" class="${k === tab ? 'on' : ''}">${l}${k === 'notices' && r.reply.notices ? ` <span>${r.reply.notices}</span>` : ''}</a>`).join('');
  const pick = Math.min(Math.max(Number(n) || 1, 1), entries.length || 1);
  let detail = '<div class="empty">Nothing waits for you.</div>';
  if (entries.length) {
    const e = entries[pick - 1];
    detail = tab === 'notices' ? notice(e) : await explain(pick, e, tab);
  }
  return head('Only what needs a person', 'Your office',
    entries.length && tab !== 'notices' ? `<button type="button" class="btn" data-run="hall inbox --clear">Dismiss all</button>` : '') + `
  <nav class="tabs" aria-label="Office views">${tabs}</nav>
  <div class="cols"><section class="narrow" aria-label="Entries, oldest first" style="flex:1 1 320px">
    ${entries.map((e, k) => `<a class="card entry${k + 1 === pick ? ' on' : ''}" href="#/office/${tab}/${k + 1}">
      <div class="row"><span class="kind ${esc(e.kind)}">${k + 1} · ${esc(cap(label(e.kind)))}${e.qid ? ' ' + esc(e.qid) : ''}</span>
      <span class="grow"></span><span class="small muted">${esc(ago(e.since))}</span></div>
      <div>${esc(e.text)}</div>${e.item ? `<div class="small muted mono">${esc(e.item)}</div>` : ''}</a>`).join('')
    || '<div class="empty">Empty.</div>'}</section>
  <section class="wide card" style="padding:20px 22px">${detail}</section></div>`;
}

function notice(e) {
  return `<div class="small muted">${esc(label(e.kind))} · ${esc(ago(e.since))}${e.item ? ` · <a href="#/item/${esc(e.item)}">${esc(e.item)}</a>` : ''}</div>
    <h2 style="font-size:15px">${esc(e.text)}</h2>
    <p class="muted" style="margin:0">The loop handles notices on its own; they never reach your office.</p>
    <div class="row">${actions(e.answer_with)}</div>`;
}

async function explain(n, e, tab) {
  const r = await hall(['inbox', String(n), ...(tab === 'all' ? ['--all'] : [])]);
  const x = errorOf(r) ? e : Object.assign({}, e, r.reply);
  const options = x.options || [];
  const question = x.kind === 'question' && x.qid;
  return `<div class="small muted">${esc(cap(label(x.kind)))} · ${esc(ago(x.since))}${x.item ? ` · <a href="#/item/${esc(x.item)}">${esc(x.item)}</a>` : ''}</div>
    <h2 style="font-size:15px">${esc(x.text)}</h2>
    ${x.why ? `<p style="margin:0">${esc(x.why)}</p>` : ''}
    ${(x.evidence || []).length ? `<ul style="margin:0;padding-left:18px">${x.evidence.map((v) => `<li>${esc(v)}</li>`).join('')}</ul>` : ''}
    ${question ? `<div style="display:flex;flex-direction:column;gap:8px">${options.map((o, k) => `
      <button type="button" class="btn opt" data-answer="${esc(o)}" data-qid="${esc(x.qid)}"><span class="mono" style="color:var(--brass)">${String.fromCharCode(97 + k)}</span>${esc(o)}
      ${x.default === o ? '<span class="small muted" style="margin-left:auto">the default</span>' : ''}</button>`).join('')}</div>
      <form data-form="answer" data-qid="${esc(x.qid)}" style="display:flex;flex-direction:column;gap:8px">
        <label class="field">${options.length ? 'Or answer in your words' : 'Your answer'}<textarea name="answer" rows="3" required></textarea></label>
        <div class="row"><button class="btn primary">Answer and resume the session</button>
        <span class="small muted">The daemon resumes the asking session with your answer.</span></div></form>` : ''}
    <div class="row">${actions((x.actions || []).filter((a) => !question || !/^hall answer /.test(a.command)))}</div>`;
}

async function queue() {
  const r = await hall(['queue']);
  if (errorOf(r)) return head('Delivery office', 'Merge queue') + failure(r, 'hall queue');
  const q = r.reply;
  const rows = q.queue || [];
  const tone = { ready: 'ok', merging: 'ok', queued: 'info', ci_pending: 'info', lit: 'you', held: 'you',
    changes_requested: 'you', ci_red: 'stop', conflicting: 'stop', behind: '', waits_for: '' };
  return head('Delivery office', 'Merge queue', `<div class="card" style="flex-direction:row;align-items:center;gap:10px;padding:8px 12px">
    Merger <span class="pill ${q.mode === 'auto' ? 'ok' : ''}">${esc(q.mode)}</span>
    <span class="small muted">${q.require_ci ? 'requires CI' : 'CI not required'}</span><a href="#/settings/merge" class="small">Change</a></div>`)
  + `<p class="muted" style="margin:0">What the merger will do, in order. Read from GitHub; this page never merges on its own.</p>
  <div class="table"><table><thead><tr><th scope="col">#</th><th scope="col">PR</th><th scope="col">Item</th><th scope="col">Base</th>
    <th scope="col">Status</th><th scope="col">Why</th></tr></thead><tbody>
    ${rows.map((x) => `<tr><td class="mono">${esc(x.position)}</td>
      <td><a href="${esc(x.pr_url)}" target="_blank" rel="noopener noreferrer">#${esc(String(x.pr_url).split('/').pop())}</a></td>
      <td><a href="#/item/${esc(x.item)}" class="mono">${esc(x.item)}</a> ${esc(x.title)}</td><td class="mono">${esc(x.base)}</td>
      <td><span class="pill ${tone[x.status] || ''}">${esc(label(x.status))}</span></td>
      <td class="muted">${esc(x.why)}${(x.waits_for || []).length ? ` · waits for ${esc(x.waits_for.join(', '))}` : ''}
      ${x.status === 'lit' || x.status === 'held' ? ` ${actions([{ command: `hall retry ${x.item} --reason "<why>"`, does: 'let it merge' }], { sm: true })}` : ''}</td></tr>`).join('')
    || '<tr><td colspan="6" class="muted">No open shipped PRs.</td></tr>'}</tbody></table></div>`;
}

async function records() {
  const args = ['history', '--limit', '100'];
  if (S.hist.rejected) args.push('--rejected');
  if (S.hist.lens) args.push('--lens', S.hist.lens);
  if (S.hist.since) args.push('--since', S.hist.since);
  const [r, st] = await Promise.all([hall(args), hall(['stats', ...(S.hist.since ? ['--since', S.hist.since] : [])])]);
  const filters = `<div class="row">
    <label class="check"><input type="checkbox" data-bind="hist.rejected" ${S.hist.rejected ? 'checked' : ''}>Rejected at least once</label>
    <label class="row small muted">Lens <select data-bind="hist.lens">${['', 'correctness', 'evidence', 'security', 'operability', 'portability', 'docs', 'repo_rules', 'design'].map((l) =>
      `<option value="${l}" ${l === S.hist.lens ? 'selected' : ''}>${l || 'any'}</option>`).join('')}</select></label>
    <label class="row small muted">Ended within <select data-bind="hist.since">${[['7d', '7 days'], ['30d', '30 days'], ['90d', '90 days'], ['', 'all time']].map(([v, l]) =>
      `<option value="${v}" ${v === S.hist.since ? 'selected' : ''}>${l}</option>`).join('')}</select></label></div>`;
  if (errorOf(r)) return head('History', 'Records') + filters + failure(r, 'hall history');
  const items = r.reply.items || [];
  return head('History', 'Records', `<span class="muted">${items.length} of ${r.reply.total ?? items.length}</span>`) + filters + statsTiles(st) + `
  <div class="table"><table><thead><tr><th scope="col">Item</th><th scope="col">Outcome</th><th scope="col">Rounds</th>
    <th scope="col">Took</th><th scope="col">Why it was rejected</th></tr></thead><tbody>
    ${items.map((x) => `<tr><td><a class="mono" href="#/item/${esc(x.id)}">${esc(x.id)}</a> ${esc(x.title)}</td>
      <td>${x.state === 'dropped' ? `<span class="pill">dropped</span> <span class="small muted">${esc(x.drop_reason || '')}</span>`
        : `<span class="pill ${x.delivered === 'merged' ? 'ok' : ''}">${esc(x.delivered || x.outcome || x.state)}</span>`}
        ${x.pr_url ? ` <a href="${esc(x.pr_url)}" target="_blank" rel="noopener noreferrer" class="small">#${esc(x.pr_url.split('/').pop())}</a>` : ''}</td>
      <td class="num">${esc(x.round)}</td><td class="mono small" style="white-space:nowrap">${esc(x.duration || '')}</td>
      <td class="small">${(x.blocking || []).slice(0, 3).map((b) => `<div><span class="muted">${esc(b.lens)}</span> ${esc(b.title)}</div>`).join('')}</td></tr>`).join('')
    || '<tr><td colspan="5" class="muted">Nothing in this range.</td></tr>'}</tbody></table></div>`;
}

// The loop's numbers (hall stats): what a change to the reviewer should move.
function statsTiles(st) {
  if (errorOf(st)) return failure(st, 'hall stats');
  const s = st.reply;
  if (!s.items || !s.items.counted) return '';
  const t = s.reviews.to_approval;
  const f = s.findings;
  const pct = (x) => `${Math.round(x || 0)}%`;
  const tile = (l, n, sub) => `<div class="card"><div class="small muted">${l}</div>
    <div style="font:700 22px var(--display)">${n}</div><div class="small muted">${sub}</div></div>`;
  return `<section class="tiles" aria-label="Review loop stats">
    ${tile('Reviews to approval', s.reviews.approved ? t.mean.toFixed(2) : '—',
    s.reviews.approved ? `median ${t.median} · one review ${pct(t.one_pct)} · three or more ${pct(t.three_or_more_pct)}` : 'nothing approved yet')}
    ${tile('Reviewer noise', pct(f.noise_pct), `${f.withdrawn} of ${f.raised} findings withdrawn`)}
    ${tile('Findings', f.raised, `${f.blocker} blocker · ${f.major} major · ${f.minor} minor · ${f.nit} nit · ${f.open} open`)}
    ${tile('Disputes', f.disputed, f.disputed ? `${f.disputes_upheld} upheld · ${f.disputes_rejected} rejected` : 'none')}
    ${tile('Items', s.items.counted, `${s.items.closed} closed · ${s.items.dropped} dropped · ${s.items.escalated} escalated`)}
  </section>`;
}

async function gazette() {
  const r = await hall(['gazette', '--list']);
  if (errorOf(r)) return head('Every finished item', 'Gazette') + failure(r, 'hall gazette --list');
  const ed = r.reply.editions || [];
  return head('Every finished item', 'Gazette', `<span class="muted">${ed.length} edition${ed.length === 1 ? '' : 's'}</span>`) + `
  <div class="table"><table><thead><tr><th scope="col">Edition</th><th scope="col">State</th><th scope="col">Printed for</th></tr></thead><tbody>
    ${ed.map((x) => `<tr><td><a class="mono" href="#/item/${esc(x.id)}/gazette">${esc(x.id)}</a> ${esc(x.title)}</td>
      <td>${pill(x.state)}</td><td class="small muted">${esc(ago(x.since))} ago</td></tr>`).join('')
    || '<tr><td colspan="3" class="muted">No editions yet: the librarian prints one when an item finishes.</td></tr>'}</tbody></table></div>`;
}

async function library() {
  const [rev, hits] = await Promise.all([hall(['library', 'review']),
    S.libq ? hall(['library', 'search', S.libq, '--proposed', '--limit', '30']) : Promise.resolve(null)]);
  const proposed = (rev && rev.reply && rev.reply.proposed) || [];
  const results = (hits && hits.reply && hits.reply.results) || [];
  return head('Kept locally, never committed', 'Library', `<div class="row">
    <button type="button" class="btn" data-run="hall library import">Import</button>
    <button type="button" class="btn" data-run="hall library export --out &lt;file&gt;">Export</button></div>`) + `
  <form data-form="libsearch" class="row"><input type="search" name="q" value="${esc(S.libq)}" placeholder="Search learnings" class="grow" aria-label="Search learnings">
    <button class="btn">Search</button></form>
  ${failure(rev, 'hall library review')}
  ${proposed.length ? `<section class="card" style="border-color:var(--brass-line);background:var(--brass-soft)">
    <div class="row"><h2 class="grow">Proposed by the librarian · ${proposed.length}</h2>
    <button type="button" class="btn sm" data-run="${esc(command(['library', 'review', '--accept', proposed.map((p) => p.id).join(',')]))}">Keep all</button></div>
    ${proposed.slice(0, 50).map((p) => `<div class="row" style="border-top:1px solid var(--brass-line);padding-top:8px">
      <div class="grow" style="flex:1 1 300px"><div class="small muted">${esc(p.id)} · ${esc(p.kind)} · ${esc(p.change)}${(p.scope || []).length ? ' · ' + esc(p.scope.join(', ')) : ''}</div>${esc(p.title)}</div>
      <button type="button" class="btn sm primary" data-run="hall library review --accept ${esc(p.id)}">Keep</button>
      <button type="button" class="btn sm" data-run="hall library review --reject ${esc(p.id)}">Reject</button></div>`).join('')}
    ${proposed.length > 50 ? `<div class="small muted">and ${proposed.length - 50} more</div>` : ''}</section>` : ''}
  ${S.libq ? `<section class="card"><h2>${results.length} learning${results.length === 1 ? '' : 's'} for “${esc(S.libq)}”</h2>
    ${failure(hits, 'hall library search')}
    ${results.map((h) => `<div class="row" style="border-top:1px solid var(--line-2);padding-top:8px">
      <div class="grow" style="flex:1 1 300px"><div class="small muted">${esc(h.id)} · ${esc(h.kind)}${h.lens ? ' · ' + esc(h.lens) : ''} · ${esc(h.status)} · used ${esc(h.used)}×</div>${esc(h.title)}</div>
      ${h.status === 'proposed' ? '' : `<button type="button" class="btn sm" data-run="hall library retire ${esc(h.id)} --reason &quot;&lt;why&gt;&quot;">Retire</button>`}</div>`).join('')}
    </section>` : '<p class="muted" style="margin:0">Search to see accepted learnings; the reviewer is briefed with the ones that match each change.</p>'}`;
}

async function usage() {
  const [b, u, d] = await Promise.all([hall(['budget']),
    hall(['usage', '--by', S.usage.by, ...(S.usage.since ? ['--since', S.usage.since] : [])]),
    hall(['usage', '--by', 'day', '--since', '14d'])]);
  const seg = (key, vals) => `<div class="row" role="group">${vals.map(([v, l]) =>
    `<button type="button" class="btn sm${S.usage[key] === v ? ' primary' : ''}" data-set="usage.${key}" data-val="${v}">${l}</button>`).join('')}</div>`;
  let caps = failure(b, 'hall budget');
  if (!errorOf(b)) {
    const bo = b.reply;
    caps = `<section class="tiles" aria-label="Budget caps">${(bo.caps || []).map((c) => {
      const pct = c.limit_tokens ? Math.min(100, (100 * (c.used || 0)) / c.limit_tokens) : 0;
      const name = c.level === 'room' ? `${label(c.room)} room, today` : c.level === 'item' ? 'Per item' : c.level === 'daily' ? 'The loop, today' : c.key;
      return `<div class="card"><div class="small muted">${esc(cap(name))} · <span class="mono">${esc(c.key)}</span></div>
        <div style="font:700 22px var(--display)">${c.used != null ? tok(c.used) : '—'} <span class="small muted" style="font-weight:400">${c.limit ? 'of ' + esc(c.limit) : 'no cap'}${c.granted ? ' +' + tok(c.granted) + ' granted' : ''}</span></div>
        ${c.limit_tokens ? `<div class="bar ${c.reached ? 'stop' : pct > 80 ? 'warn' : ''}"><span style="width:${pct}%"></span></div>` : ''}
        ${c.used_usd != null ? `<div class="small muted">${usd(c.used_usd)}${c.reached ? ' · <b style="color:var(--stop)">reached</b>' : ''}</div>` : ''}</div>`;
    }).join('')}
    <div class="card"><div class="small muted">Per run</div>${(bo.runs || []).map((x) => `<div class="row small"><span class="grow">${esc(x.role)} · ${esc(String(x.model || '').replace(/^claude-/, ''))}${x.effort ? ' · ' + esc(x.effort) : ''}</span>
      <span class="mono">${x.max_usd != null ? usd(x.max_usd) : 'no cap'} · ${Math.round((x.timeout_secs || 0) / 60)}m</span></div>`).join('')}</div></section>`;
  }
  const days = (d.reply && d.reply.rows) || [];
  days.sort((a, x) => String(a.key).localeCompare(String(x.key)));
  const max = Math.max(1, ...days.map((x) => x.tokens || 0));
  const chart = days.length ? `<div class="chart" role="img" aria-label="Tokens per day, last 14 days">${days.map((x) =>
    `<div class="col" tabindex="0"><span style="height:${(100 * (x.tokens || 0)) / max}%"></span>
      <div class="tip">${esc(x.key)} · ${tok(x.tokens)} tokens · ${usd(x.cost_usd)}</div></div>`).join('')}</div>
    <div class="axis"><span>${esc(days[0].key)}</span><span>${esc(days[days.length - 1].key)}</span></div>` : '<div class="empty">No usage in the last 14 days.</div>';
  return head('Tokens and list-price cost', 'Usage & budget', `<div class="row">
    <button type="button" class="btn" data-run="hall budget --grant" data-confirm="Let the loop go past today's daily cap, without changing the cap? It ends at 00:00 UTC.">Grant more today</button>
    <button type="button" class="btn" data-run="hall budget --set &quot;&lt;key=value, such as daily=4M or review.max_usd=3&gt;&quot;">Set a cap</button></div>`)
  + caps + `<section class="card"><h2>Tokens per day, everything</h2>${chart}</section>
  <section class="card"><div class="row"><h2 class="grow">Grouped</h2>
    ${seg('by', [['room', 'Room'], ['role', 'Role'], ['model', 'Model'], ['item', 'Item'], ['day', 'Day'], ['source', 'Source']])}
    ${seg('since', [['24h', '24h'], ['7d', '7d'], ['14d', '14d'], ['30d', '30d'], ['', 'All']])}</div>
    ${errorOf(u) ? failure(u, 'hall usage') : usageTable(u.reply, cap(S.usage.by))}</section>`;
}

async function settings(group) {
  if (group) S.cfg.group = group;
  const [cfg, daemon, doctor, ext] = await Promise.all([
    hall(['config', 'list', ...(S.cfg.user ? ['--user'] : [])]), hall(['daemon', 'status']),
    hall(['doctor']), hall(['extensions'])]);
  const entries = (cfg.reply && cfg.reply.entries) || [];
  const groups = [...new Set(entries.map((e) => e.key.split('.')[0]))];
  const top = entries.filter((e) => !e.key.includes('.'));
  const tabs = ['', ...groups.filter((g) => entries.some((e) => e.key.startsWith(g + '.')))];
  const g = tabs.includes(S.cfg.group) ? S.cfg.group : '';
  const shown = g ? entries.filter((e) => e.key.startsWith(g + '.')) : top;
  const rows = shown.map((e, k) => {
    const id = `cfg-${k}`;
    const v = e.value == null ? '' : typeof e.value === 'object' ? JSON.stringify(e.value) : String(e.value);
    const input = e.kind === 'bool'
      ? `<select id="${id}"><option ${v === 'true' ? 'selected' : ''}>true</option><option ${v === 'false' ? 'selected' : ''}>false</option></select>`
      : e.kind === 'choice' && (e.values || []).length
        ? `<select id="${id}"><option value=""></option>${e.values.map((o) => `<option ${o === v ? 'selected' : ''}>${esc(o)}</option>`).join('')}</select>`
        : `<input id="${id}" value="${esc(v)}" placeholder="${e.value == null ? 'unset' : ''}" class="mono"${(e.values || []).length ? ` list="${id}-l"` : ''}>
          ${(e.values || []).length ? `<datalist id="${id}-l">${e.values.map((o) => `<option value="${esc(o)}">`).join('')}</datalist>` : ''}`;
    return `<div class="cfg"><label for="${id}"><div class="mono">${esc(e.key)}</div>${e.help ? `<div class="help">${esc(e.help)}</div>` : ''}</label>
      ${input}<div class="row"><span class="src ${esc(e.source)}">${esc(e.source)}</span>
      <button type="button" class="btn sm" data-cfg="${esc(e.key)}" data-input="${id}">Save</button>
      ${e.source === 'repo' || e.source === 'user' ? `<button type="button" class="btn sm" data-run="${esc(command(['config', 'unset', e.key, ...(S.cfg.user ? ['--user'] : [])]))}">Reset</button>` : ''}</div></div>`;
  }).join('');
  const dm = daemon.reply || {};
  const paused = dm.state && dm.state.paused;
  const checks = (doctor.reply && doctor.reply.checks) || [];
  const exts = (ext.reply && ext.reply.extensions) || [];
  return head(S.cfg.user ? 'Your user config' : '.hallmark.toml', 'Settings', `<label class="check"><input type="checkbox" data-bind="cfg.user" ${S.cfg.user ? 'checked' : ''}>Your user config (tools, notifications)</label>`) + `
  <div class="cols"><section class="wide card">
    <nav class="tabs" aria-label="Setting groups">${tabs.map((t) => `<a href="#/settings/${esc(t)}" class="${t === g ? 'on' : ''}">${esc(t || 'General')}</a>`).join('')}</nav>
    ${failure(cfg, 'hall config list')}${rows || '<div class="empty">No settings here.</div>'}
    <div class="small muted">${esc(S.cfg.user ? (cfg.reply && cfg.reply.user_file) || '' : (cfg.reply && cfg.reply.repo_file) || '')}</div></section>
  <aside class="narrow">
    <section class="card"><h2>Daemon</h2>
      <div class="row"><span class="dot ${dm.running ? (paused ? 'warn' : 'ok') : 'stop'}"></span><b>${dm.running ? (paused ? 'Paused' : 'Running') : 'Stopped'}</b>
        <span class="small muted">${paused ? esc(paused.reason || '') + (paused.until ? ' until ' + esc(clock(paused.until)) : '') : dm.state && dm.state.pid ? 'pid ' + esc(dm.state.pid) : ''}</span></div>
      <div class="small muted">${(dm.repos || []).length} repos · ${dm.at_login ? 'starts at login' : 'not started at login'}</div>
      <div class="row">${dm.running ? '<button type="button" class="btn" data-run="hall daemon stop">Stop</button>' : '<button type="button" class="btn primary" data-run="hall daemon start">Start</button>'}
        ${paused ? '<button type="button" class="btn" data-run="hall daemon resume">Lift the pause</button>' : ''}
        ${dm.at_login ? '<button type="button" class="btn" data-run="hall daemon stop --at-login">Don\'t start at login</button>' : '<button type="button" class="btn" data-run="hall daemon start --at-login">Start at login</button>'}</div></section>
    <section class="card"><div class="row"><h2 class="grow">Doctor</h2><button type="button" class="btn sm" data-refresh>Check again</button></div>
      ${checks.map((c) => `<div class="small" style="display:grid;grid-template-columns:16px minmax(0,1fr);gap:4px 6px;overflow-wrap:anywhere">
        <span style="color:var(${c.ok ? '--ok' : '--stop'})">${c.ok ? '✓' : '✗'}</span>
        <div><b>${esc(c.name)}</b> <span class="muted">${esc(c.detail)}</span>
        ${c.fix ? `<div class="row" style="margin-top:4px">${runnable(c.fix) ? `<button type="button" class="btn sm" data-run="${esc(c.fix)}">Fix it</button>` : ''}<code>${esc(c.fix)}</code></div>` : ''}</div></div>`).join('')}
      ${doctor.reply && doctor.reply.healthy === false ? `<div class="row">${actions(doctor.reply.next, { sm: true })}</div>` : ''}</section>
    <section class="card"><h2>Extensions</h2>${exts.map((x) => `<div class="small"><b>${esc(x.name)}</b> · ${esc(x.credit)}</div>
      <div class="row">${(x.roles || []).map((r) => `<label class="check small"><input type="checkbox" ${r.on ? 'checked' : ''}
        data-toggle="${esc(command(['extensions', r.on ? 'disable' : 'enable', x.id, '--role', r.role]))}">${esc(r.role)}</label>`).join('')}</div>`).join('') || '<div class="empty">None.</div>'}</section>
    <section class="card"><h2>Notifications</h2><p class="small muted" style="margin:0">Desktop, phone (ntfy, Telegram) and Slack: set them under the notify settings in your user config.</p>
      <div class="row"><button type="button" class="btn" data-run="hall notify test">Send a test</button></div></section>
    <section class="card"><h2>Index</h2><p class="small muted" style="margin:0">The SQLite index is a cache of the journal.</p>
      <div class="row"><button type="button" class="btn" data-run="hall reindex">Rebuild it</button></div></section>
  </aside></div>`;
}

function newItem() {
  const m = S.newMode;
  const modes = [['request', 'A request'], ['issue', 'GitHub issue'], ['follow-up', 'Feedback on a PR'], ['adopt', 'Take over a PR']];
  const field = (name, lbl, ph, extra = '') => `<label class="field">${lbl}<input name="${name}" placeholder="${esc(ph)}" ${extra}></label>`;
  return head('Front desk', 'New item') + `<div class="cols"><form class="wide card" data-form="new" style="padding:22px 24px;gap:14px">
    <div class="row" role="group" aria-label="Start from">${modes.map(([k, l]) =>
      `<button type="button" class="btn sm${k === m ? ' primary' : ''}" data-set="newMode" data-val="${k}">${l}</button>`).join('')}</div>
    ${m === 'request' ? `<label class="field">The request, in your words<textarea name="request" rows="5" required></textarea>
      <span class="small muted" style="font-weight:400">It becomes the first journal entry. The Front Desk splits it into parallel parts when it can.</span></label>` : ''}
    ${m === 'issue' ? field('issue', 'Issue number', '212', 'required inputmode="numeric" pattern="[0-9]+"') : ''}
    ${m === 'follow-up' ? field('follow_up', 'The shipped PR', 'a URL or a number', 'required') : ''}
    ${m === 'adopt' ? field('pr', 'The open PR', 'a URL or a number', 'required') + field('base', 'Review against (optional)', 'its base') : ''}
    ${field('title', 'Title (optional)', m === 'issue' ? "the issue's title" : 'the request')}
    ${m !== 'adopt' ? `<details><summary style="cursor:pointer;min-height:32px">More options</summary>
      <div class="cols" style="margin-top:10px">${field('branch', 'Branch', 'from the branch setting')}${field('parent', 'A part of (an item ID)', 'H-0042')}</div>
      <label class="check"><input type="checkbox" name="no_split">Keep it one item: never split</label></details>` : ''}
    <div class="row"><button class="btn primary">${m === 'adopt' ? 'Adopt it' : 'Create it'}</button><a class="btn" href="#/floor">Cancel</a></div>
    <div id="new-result"></div></form>
  <aside class="narrow card"><h2>What happens</h2><p class="muted" style="margin:0">hall makes the item, its branch and its worktree.
    Then open Claude Code there and build it; the review loop takes over once the agent says it looks done.</p>
    <p class="small muted" style="margin:0">The same, from a terminal: <code>hall new "&lt;request&gt;"</code></p></aside></div>`;
}

async function submitNew(form) {
  const f = new FormData(form);
  const v = (k) => String(f.get(k) || '').trim();
  let args;
  if (S.newMode === 'adopt') {
    args = ['adopt', '--pr', v('pr')];
    if (v('base')) args.push('--base', v('base'));
  } else {
    args = ['new'];
    if (S.newMode === 'request') args.push(v('request'));
    if (S.newMode === 'issue') args.push('--issue', v('issue'));
    if (S.newMode === 'follow-up') args.push('--follow-up', v('follow_up'));
    if (v('branch')) args.push('--branch', v('branch'));
    if (v('parent')) args.push('--parent', v('parent'));
    if (f.get('no_split')) args.push('--no-split');
  }
  if (v('title')) args.push('--title', v('title'));
  const out = $('#new-result');
  out.innerHTML = '<div class="row muted"><span class="spin"></span> The Front Desk is reading it…</div>';
  S.running++;
  const r = await hall(args);
  S.running--;
  if (errorOf(r)) { out.innerHTML = failure(r, command(args)); return; }
  const x = r.reply;
  const kids = x.children || [];
  out.innerHTML = `<div class="card" style="border-color:var(--ok)"><b>${esc(x.id)} · ${esc(x.title)}</b>
    ${x.plan && x.plan.reason ? `<div class="small muted">${esc(x.plan.reason)}</div>` : ''}
    ${kids.length ? `<div>${kids.length} parallel parts: ${kids.map((k) => `<a href="#/item/${esc(k.id)}">${esc(k.id)}</a> ${esc(k.title)}`).join(' · ')}</div>` : ''}
    ${x.open ? copyable(x.open, 'open Claude Code there') : ''}
    <div class="row"><a class="btn primary" href="#/item/${esc(x.id)}">Open ${esc(x.id)}</a></div></div>`;
  sidebar();
}

// --------------------------------------------------------------- the shell

const VIEWS = { floor, item, office, queue, records, gazette, library, usage, settings, new: newItem };
// The views that read again on every refresh; the others wait for you.
const LIVE = new Set(['floor', 'item', 'office', 'queue', 'usage']);

const route = () => (location.hash.replace(/^#\/?/, '') || 'floor').split('/').map(decodeURIComponent);
const repoName = () => (S.repos.find((r) => r.path === S.repo) || {}).name || 'hall';

async function draw() {
  if (!S.token || S.locked) return locked();
  if (!S.repo) {
    $('#main').innerHTML = head('Hallmark', 'No repositories yet') + '<p>Run <code>hall init</code> in a git repository, then reload.</p>';
    return;
  }
  const seq = ++S.seq;
  const [view, ...rest] = route();
  const fn = VIEWS[view] || floor;
  for (const a of document.querySelectorAll('#nav a')) a.classList.toggle('on', a.dataset.view === (view === 'item' ? 'floor' : view));
  const [title, note] = SUB[view] || SUB.floor;
  $('#sub-title').textContent = title;
  $('#sub-note').textContent = note;
  let html;
  S.drawing = true;
  try { html = await fn(...rest); } catch (e) { html = `<div class="card">Something went wrong drawing this page: ${esc(e.message)}</div>`; }
  S.drawing = false;
  S.lastTick = Date.now();
  if (seq !== S.seq) return;
  $('#main').innerHTML = html;
  S.lastDraw = Date.now();
  S.drawn = [view, rest[0]];
}

// ---------------------------------------------------------------- the shell

// What the sub-bar says on each view.
const SUB = {
  floor: ['Live floor.', 'Cost is an estimate at list price from token counts, not a bill.'],
  item: ['Item.', 'Every button runs the hall command it names, as you would in a terminal.'],
  office: ['Your office.', 'Only what needs a person: questions, “looks done?”, escalations and holds.'],
  queue: ['Merge queue.', 'What the merger will do, in order. Read from GitHub; this page never merges on its own.'],
  records: ['Records.', 'Closed and dropped items, and why things were rejected.'],
  gazette: ['Gazette.', 'A newspaper page for each finished item, written up by the librarian for people.'],
  library: ['Library.', 'Learnings from past reviews, kept on this machine, never committed.'],
  usage: ['Usage & budget.', 'Cost is an estimate at list price from token counts, not a bill.'],
  settings: ['Settings.', 'Each change runs hall config; comments and layout in the file are kept.'],
  new: ['New item.', 'The Front Desk makes the item, its branch and its worktree.'],
};

// Reads what the rail, the feed and the chips show: the items, the HUD
// (the loop, the inbox, the daemon's log) and, now and then, the daemon.
async function refreshShell() {
  if (!S.token || !S.repo || S.locked) return;
  const repo = S.repo;
  const [st, h] = await Promise.all([hall(['status']), hall(['hud', '--lines', '150'])]);
  if (S.ticks++ % 3 === 0 || !S.daemon) {
    const dm = await hall(['daemon', 'status']);
    S.daemon = dm.reply || {};
  }
  if (repo !== S.repo) return;
  const hud = h.reply || {};
  const needs = new Set((hud.needs_you || []).map((x) => (typeof x === 'string' ? x : x.item || x.id)).filter(Boolean));
  S.data = {
    error: errorOf(st) ? st : null,
    items: (st.reply && st.reply.items) || [],
    hud,
    loop: new Map((hud.loop_items || []).map((i) => [i.id, i])),
    needs,
  };
  drawShell();
}
const sidebar = () => refreshShell();

function drawShell() {
  if (!S.data) return;
  rail();
  feed();
  chips();
}

function rail() {
  const d = S.data;
  const [view, id] = route();
  const items = d.items.slice().sort((a, b) => ROOMS.indexOf(roomOf(a.state)) - ROOMS.indexOf(roomOf(b.state)) || String(a.id).localeCompare(String(b.id)));
  const inDev = items.filter((i) => i.state === 'development').length;
  const rows = items.map((i) => {
    const [tone] = band(i);
    const room = roomOf(i.state);
    const stage = stageOf(i);
    const dot = tone === 'run' ? 'ok' : tone === 'stop' ? 'stop' : tone === 'you' ? 'warn' : tone === 'dev' ? 'ring' : '';
    const rj = i.rejects || 0;
    return `<a class="rrow${view === 'item' && id === i.id ? ' on' : ''}${i.state === 'escalated' ? ' stop' : ''}" href="#/item/${esc(i.id)}" title="${esc(i.title)}">
      <div class="rr-1"><span class="dot ${dot}"></span><span class="rr-t">${esc(i.title)}</span>
        ${rj ? `<span class="chip sq-chip ${rj >= 3 ? 'stop' : 'warn'}">⚠ ${rj}×</span>` : ''}
        <span class="chip sq-chip"><i class="sq" style="--c:${room[2]}"></i>${esc(label(i.state))}</span></div>
      <div class="rr-2"><span class="mono small muted">${esc(i.id)}</span><span class="pbar${tone === 'stop' ? ' stop' : tone === 'you' ? ' warn' : ''}"><span style="width:${(100 * stage) / 7}%"></span></span>
        <span class="mono small">${stage}/7</span>${i.round ? `<span class="mono small muted">r${i.round}</span>` : ''}</div></a>`;
  }).join('');
  $('#rail').innerHTML = `<div class="col-head"><b>Items</b><span class="grow"></span><span>${items.length}</span>
      <button type="button" class="icon" data-collapse="rail" aria-label="Show or hide the items">${S.noRail ? '›' : '‹'}</button></div>
    <a class="rrow all${view === 'floor' ? ' on' : ''}" href="#/floor"><b>All items</b>
      <span class="small muted">${d.loop.size} in the loop · ${d.hud.inbox || 0} waiting · ${inDev} in development</span></a>
    ${rows || '<div class="empty" style="padding:12px">No open items.</div>'}
    <div class="feed-foot"><a href="#/records">Closed and dropped items</a></div>`;
}

// One line of the daemon's log: "2026-10-18T18:00:54Z H-0086 checks 2/3: …".
function feedLine(l) {
  const [t, ...rest] = l.split(' ');
  let text = rest.join(' ');
  const m = text.match(/\bH-\d{4,}\b/);
  const id = m ? m[0] : null;
  if (id) text = text.replace(id, '').replace(/^[\s:]+/, '').replace(/\s{2,}/g, ' ');
  const role = text.match(/^(\w[\w-]*):/);
  const who = role ? role[1] : '';
  const c = /assayer|review|verdict|approv/i.test(text) ? 'var(--principal)'
    : /fixer|fix run|resubmitted/i.test(text) ? 'var(--fixer)'
      : /ship|merge|deliver|PR |pull\/|sweeper/i.test(text) ? 'var(--ship)'
        : /checks|gate/i.test(text) ? 'var(--info)'
          : /notif|librar|distill/i.test(who + text) ? 'var(--clerk)' : 'var(--dev)';
  const tone = /\bfail|\bred\b|\bblocker|\bconflict/i.test(text) ? 'stop' : /\bescalat|\bheld\b|\bhold\b|\bstuck\b|\bwaiting\b/i.test(text) ? 'warn' : '';
  return { t, id, text, c, tone };
}

function feed() {
  const lines = (S.data.hud.live || []).slice().reverse();
  const shown = lines.slice(0, S.feedShown).map(feedLine);
  $('#feed').innerHTML = `<div class="col-head"><button type="button" class="icon" data-collapse="feed" aria-label="Show or hide the activity">${S.noFeed ? '‹' : '›'}</button>
      <b>Activity · ${esc(repoName())}</b><span class="grow"></span><span>newest first</span></div>
    ${shown.map((f) => `<div class="frow ${f.tone}"><span class="t mono">${esc(clock24(f.t))}</span><i class="sq" style="--c:${f.c}"></i>
      <div class="x">${f.id ? `<b>${esc(f.id)}</b> ` : ''}${esc(f.text)}${f.id ? `<a class="sub" href="#/item/${esc(f.id)}">↗ Open item</a>` : ''}</div></div>`).join('')
      || '<div class="empty" style="padding:12px">The daemon has logged nothing yet.</div>'}
    <div class="feed-foot"><div class="row"><span class="grow">Showing ${shown.length} of ${lines.length}</span>
      ${lines.length > shown.length ? '<button type="button" class="btn sm" data-more>Show 30 more</button>' : ''}</div>
      <span>From the daemon's log: the latest ${lines.length} lines are read.</span></div>`;
}
const clock24 = (iso) => (iso && !Number.isNaN(Date.parse(iso)) ? new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }) : '');

function chips() {
  const d = S.daemon || {};
  const hud = S.data ? S.data.hud : {};
  const paused = d.state && d.state.paused;
  const chip = $('#daemon-chip');
  chip.className = 'chip' + (!d.running ? ' stop' : paused ? ' warn' : '');
  chip.innerHTML = `<span class="dot ${d.running ? (paused ? 'warn' : 'ok') : 'stop'}"></span>${d.running ? (paused ? 'Daemon paused' : 'Daemon running') : 'Daemon stopped'}
    ${!d.running ? '<button type="button" class="btn sm" data-run="hall daemon start" style="min-height:20px">Start</button>'
      : paused ? '<button type="button" class="btn sm" data-run="hall daemon resume" style="min-height:20px">Lift the pause</button>' : ''}`;
  chip.title = paused ? paused.reason || '' : (hud.daemon && hud.daemon.current) || '';
  const badge = $('#badge-inbox');
  badge.hidden = !hud.inbox;
  badge.textContent = hud.inbox || '';
  const u = S.update;
  $('#update-chip').innerHTML = u && u.status === 'available'
    ? `<span class="chip warn">hall ${esc(u.latest_version || short(u.latest))} is available
        <button type="button" class="btn sm primary" data-update style="min-height:20px">Update and restart</button></span>`
    : u ? `<span class="chip" title="${esc(u.hall || '')}">hall ${esc(u.installed_version || short(u.installed))}</span>` : '';
}

async function checkUpdate() {
  if (!S.token || !S.repo) return;
  const r = await hall(['update', '--check']);
  S.update = errorOf(r) ? null : r.reply;
  chips();
}

async function update(btn) {
  const u = S.update || {};
  const ok = await ask(`Install hall ${u.latest_version || ''}?`, [],
    'hall builds the new version, refreshes the Claude Code plugin and restarts the daemon. This page reconnects when it is done; it can take a few minutes.');
  if (!ok) return;
  btn.disabled = true;
  btn.innerHTML = '<span class="spin"></span> Updating…';
  S.running++;
  const r = await hall(['update']);
  S.running--;
  const e = errorOf(r);
  if (e) { toast(`${e.code}: ${e.message}`, true); btn.disabled = false; btn.textContent = 'Update and restart'; return; }
  btn.innerHTML = '<span class="spin"></span> Restarting…';
  const before = (await api('/api/ping')).version;
  await api('/api/restart', {});
  for (let k = 0; k < 120; k++) {
    await new Promise((res) => setTimeout(res, 1000));
    const p = await api('/api/ping');
    if (p.ok && p.version !== before) { location.reload(); return; }
    if (p.ok && k > 5) { location.reload(); return; }
  }
  toast('hall web did not come back: run `hall web` again.', true);
}

function locked() {
  $('#main').innerHTML = `<div class="card locked"><h1>Open the address hall web printed</h1>
    <p style="margin:0">This page needs the token in that address. Run <code>hall web</code> again if you lost it.</p></div>`;
}

async function loadRepos() {
  const r = await api('/api/repos');
  if (r.error) return;
  S.repos = r.repos || [];
  const saved = load('hall-repo');
  const here = S.repos[r.here] || S.repos[0];
  S.repo = (S.repos.find((x) => x.path === saved) || here || {}).path || null;
  $('#repo').innerHTML = S.repos.map((x) => `<option value="${esc(x.path)}" ${x.path === S.repo ? 'selected' : ''}>${esc(x.name)}</option>`).join('');
}

// Per-viewer conveniences: the theme, the refresh, the columns, the repo.
function load(k) { try { return localStorage.getItem(k); } catch (_) { return null; } }
function save(k, v) { try { localStorage.setItem(k, v); } catch (_) { /* no storage */ } }

function applyTheme(t) {
  S.theme = t;
  if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
  for (const b of document.querySelectorAll('[data-theme-set]')) b.classList.toggle('on', b.dataset.themeSet === t);
  save('hall-theme', t);
}

function applyColumns() {
  $('#body').classList.toggle('no-rail', S.noRail);
  $('#body').classList.toggle('no-feed', S.noFeed);
}

function setPath(path, value) {
  const keys = path.split('.');
  let o = S;
  while (keys.length > 1) o = o[keys.shift()];
  o[keys[0]] = value;
}

// The floor's controls redraw from what was read, without reading again.
function redrawFloor() {
  const fb = $('#floor-body');
  if (fb) fb.innerHTML = floorBody();
}

document.addEventListener('click', async (ev) => {
  const t = ev.target;
  const ansBtn = t.closest('[data-answer]');
  if (ansBtn) {
    ev.preventDefault();
    await run('hall answer', { args: ['answer', ansBtn.dataset.qid, '--', ansBtn.dataset.answer], button: ansBtn });
    return;
  }
  const runBtn = t.closest('[data-run]');
  if (runBtn) {
    ev.preventDefault();
    await run(runBtn.dataset.run, { button: runBtn, confirm: runBtn.dataset.confirm });
    return;
  }
  const copyBtn = t.closest('[data-copy]');
  if (copyBtn) {
    try { await navigator.clipboard.writeText(copyBtn.dataset.copy); toast('Copied'); } catch (_) { toast(copyBtn.dataset.copy); }
    return;
  }
  const setBtn = t.closest('[data-set]');
  if (setBtn) {
    setPath(setBtn.dataset.set, setBtn.dataset.val);
    if ('local' in setBtn.dataset) redrawFloor(); else draw();
    return;
  }
  const themeBtn = t.closest('[data-theme-set]');
  if (themeBtn) {
    applyTheme(themeBtn.dataset.themeSet);
    if (route()[2] === 'gazette') draw();
    return;
  }
  const col = t.closest('[data-collapse]');
  if (col) {
    const k = col.dataset.collapse === 'rail' ? 'noRail' : 'noFeed';
    S[k] = !S[k];
    save('hall-' + k, S[k] ? '1' : '');
    applyColumns();
    drawShell();
    return;
  }
  if (t.closest('[data-more]')) { S.feedShown += 30; feed(); return; }
  const cfgBtn = t.closest('[data-cfg]');
  if (cfgBtn) {
    const val = document.getElementById(cfgBtn.dataset.input).value;
    const user = S.cfg.user ? ['--user'] : [];
    const args = val === '' ? ['config', 'unset', ...user, cfgBtn.dataset.cfg]
      : ['config', 'set', ...user, cfgBtn.dataset.cfg, '--', val];
    await run(command(args), { button: cfgBtn });
    return;
  }
  if (t.closest('[data-refresh]')) { draw(); return; }
  const up = t.closest('[data-update]');
  if (up) update(up);
});

document.addEventListener('input', (ev) => {
  if (ev.target.id === 'q') {
    S.q = ev.target.value;
    const c = $('#cards');
    if (c) c.innerHTML = cardsHtml();
  }
});

document.addEventListener('change', async (ev) => {
  const el = ev.target;
  if (el.id === 'repo') {
    S.repo = el.value;
    S.data = null;
    S.feedShown = 30;
    save('hall-repo', S.repo);
    draw(); refreshShell(); checkUpdate();
    return;
  }
  if (el.id === 'refresh') { S.refresh = Number(el.value); save('hall-refresh', el.value); return; }
  if (el.dataset.bind) {
    setPath(el.dataset.bind, el.type === 'checkbox' ? el.checked : el.value);
    if ('local' in el.dataset) redrawFloor(); else draw();
    return;
  }
  if (el.dataset.toggle) await run(el.dataset.toggle);
});

document.addEventListener('submit', async (ev) => {
  const form = ev.target.closest('[data-form]');
  if (!form) return;
  ev.preventDefault();
  const kind = form.dataset.form;
  if (kind === 'new') submitNew(form);
  if (kind === 'libsearch') { S.libq = String(new FormData(form).get('q') || '').trim(); draw(); }
  if (kind === 'answer') {
    const text = String(new FormData(form).get('answer') || '').trim();
    if (text) await run('hall answer', { args: ['answer', form.dataset.qid, '--', text], button: form.querySelector('button'), keepOnError: true });
  }
});

window.addEventListener('hashchange', () => {
  const [view, id] = route();
  const [was, wasId] = S.drawn || [];
  // A new page says it's loading; a tab of the same item keeps the item.
  if (view !== was || (view === 'item' && id !== wasId)) {
    $('#main').innerHTML = '<p class="row muted"><span class="spin"></span> Loading…</p>';
    $('#main').scrollTo(0, 0);
  }
  draw();
  if (S.data) rail();
});

// Reads again every "Refresh every" seconds (a run that is going, every
// two), but never redraws under someone's hands: not while a dialog is
// open, a command runs, or a field in the page has focus.
setInterval(() => {
  if (!S.refresh || document.hidden || $('#dialog').open || S.running || S.drawing) return;
  const [view, , tab] = route();
  const every = view === 'item' && tab === 'runs' ? Math.min(S.refresh, 2) : S.refresh;
  if (Date.now() - S.lastTick < every * 1000) return;
  S.lastTick = Date.now();
  // A page being read isn't reprinted under the reader.
  if (view === 'item' && tab === 'gazette') return refreshShell();
  const focus = document.activeElement;
  const typing = focus && $('#main').contains(focus) && /^(INPUT|TEXTAREA|SELECT)$/.test(focus.tagName);
  if (LIVE.has(view) && !typing) {
    draw();
    if (view !== 'floor') refreshShell();
  } else {
    refreshShell();
  }
}, 500);
setInterval(checkUpdate, 60 * 60 * 1000);

(async () => {
  applyTheme(load('hall-theme') || 'system');
  S.refresh = Number(load('hall-refresh') ?? 5);
  if (Number.isNaN(S.refresh)) S.refresh = 5;
  $('#refresh').value = String(S.refresh);
  S.noRail = !!load('hall-noRail');
  S.noFeed = !!load('hall-noFeed');
  applyColumns();
  readToken();
  if (!S.token) return locked();
  await loadRepos();
  draw();
  if (route()[0] !== 'floor') refreshShell();
  checkUpdate();
})();
