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
  'answer', 'usage', 'reindex', 'config', 'findings', 'merge', 'history', 'queue', 'export',
  'budget', 'notify', 'library', 'extensions', 'hud', 'doctor', 'new', 'adopt', 'trace',
  'daemon', 'update']);
// Readers: offered as links to the view that shows them, not as buttons.
const READERS = new Set(['status', 'log', 'findings', 'trace', 'history', 'queue', 'usage',
  'hud', 'inbox', 'watch']);

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

const LANES = [
  ['Workshop', 'var(--dev)', ['development']],
  ['Your office', 'var(--you)', ['awaiting_you', 'escalated']],
  ['Assay office', 'var(--principal)', ['checks', 'review']],
  ['Repair shop', 'var(--fixer)', ['fixing']],
  ['Dispatch & delivery', 'var(--ship)', ['hallmarked', 'closed']],
];

async function floor() {
  const [st, hud, inbox] = await Promise.all([
    hall(['status']), hall(['hud', '--lines', '8']), hall(['inbox'])]);
  if (errorOf(st)) return head(repoName(), 'The floor') + failure(st, 'hall status');
  const items = st.reply.items || [];
  const h = hud.reply || {};
  const loop = new Map((h.loop_items || []).map((i) => [i.id, i]));
  const entries = (inbox.reply && inbox.reply.entries) || [];
  const today = h.today || {};
  const capT = today.daily_cap;
  const lanes = LANES.map(([name, color, states]) => {
    const mine = items.filter((i) => states.includes(i.state));
    return `<div class="lane"><h2><span class="swatch" style="background:${color}"></span>${esc(name)}
      <span class="muted" style="font-weight:400">${mine.length}</span></h2>
      ${mine.map((i) => card(i, loop.get(i.id))).join('') || '<div class="empty">Nothing here.</div>'}</div>`;
  }).join('');
  const live = (h.live || []).map((l) => {
    const [t, ...rest] = l.split(' ');
    return `<div><span style="color:var(--term-dim)">${esc(clock(t))}</span> ${esc(rest.join(' '))}</div>`;
  }).join('');
  return head(repoName(), 'The floor', `<a class="btn primary" href="#/new">New item</a>`) + `
  <section class="tiles" aria-label="Summary">
    <a class="card tile${entries.length ? ' you' : ''}" href="#/office"><span class="small muted">Waiting for you</span>
      <span class="n">${entries.length}</span><span class="small muted">${esc(kinds(entries))}</span></a>
    <div class="card tile"><span class="small muted">In the review loop</span><span class="n">${loop.size}</span>
      <span class="small muted">${esc(h.daemon && h.daemon.current ? h.daemon.current : 'nothing running')}</span></div>
    <div class="card tile"><span class="small muted">Open items</span><span class="n">${items.length}</span>
      <span class="small muted"><a href="#/records">closed and dropped ones</a></span></div>
    <a class="card tile" href="#/usage"><span class="small muted">Today, the loop</span>
      <span class="n">${tok(today.loop_tokens)} <span class="small muted" style="font-weight:400">${capT ? 'of ' + tok(capT) : 'no cap'}</span></span>
      <span class="small muted">${usd(today.loop_usd)} · everything ${usd(today.cost_usd)}</span></a>
  </section>
  <section class="board" aria-label="Board">${lanes}</section>
  <section class="term" aria-label="Live"><div class="row" style="color:var(--term-ink);font-family:var(--body);font-weight:600">
    <span class="grow">Live · the daemon's log</span><a href="#/settings" style="font-weight:400">Daemon</a></div>
    ${live || '<div style="color:var(--term-dim)">Nothing yet.</div>'}</section>`;
}

function kinds(entries) {
  const n = {};
  for (const e of entries) n[e.kind] = (n[e.kind] || 0) + 1;
  return Object.entries(n).map(([k, v]) => `${v} ${label(k)}`).join(' · ') || 'nothing';
}

function card(i, loop) {
  const now = loop ? loop.now : (i.summary || i.room);
  const busy = loop && /running|review|fix/i.test(loop.now || '');
  const pr = i.pr_url ? ` · PR #${esc(i.pr_url.split('/').pop())}` : '';
  return `<a class="card item${busy ? ' running' : ''}" href="#/item/${esc(i.id)}">
    <div class="id"><span>${esc(i.id)}</span><span>${esc(label(i.state))}${i.round ? ' · round ' + i.round : ''}${pr}</span></div>
    <div class="t">${esc(i.title)}</div>
    <div class="now">${esc(now || '')}</div></a>`;
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
  const tabs = ['journal', 'findings', 'diff', 'runs', 'usage'].map((t) =>
    `<a href="#/item/${esc(id)}/${t}" class="${t === tab ? 'on' : ''}">${cap(t)}</a>`).join('');
  const body = await ({ journal, findings, diff, runs, itemUsage }[tab === 'usage' ? 'itemUsage' : tab] || journal)(id, arg, i);
  return `<div class="small muted"><a href="#/floor">The floor</a> / <span class="mono">${esc(id)}</span></div>
  <header class="head"><div style="display:flex;flex-direction:column;gap:8px">
    <div class="row"><span class="mono muted">${esc(id)}</span>${pill(i.state)}<span class="small muted">${esc(i.room)} · since ${esc(ago(i.state_since))}</span></div>
    <h1 style="font-size:28px">${esc(i.title)}</h1>
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
    <h2 style="font-size:20px">${esc(e.text)}</h2>
    <p class="muted" style="margin:0">The loop handles notices on its own; they never reach your office.</p>
    <div class="row">${actions(e.answer_with)}</div>`;
}

async function explain(n, e, tab) {
  const r = await hall(['inbox', String(n), ...(tab === 'all' ? ['--all'] : [])]);
  const x = errorOf(r) ? e : Object.assign({}, e, r.reply);
  const options = x.options || [];
  const question = x.kind === 'question' && x.qid;
  return `<div class="small muted">${esc(cap(label(x.kind)))} · ${esc(ago(x.since))}${x.item ? ` · <a href="#/item/${esc(x.item)}">${esc(x.item)}</a>` : ''}</div>
    <h2 style="font-size:22px">${esc(x.text)}</h2>
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
  const r = await hall(args);
  const filters = `<div class="row">
    <label class="check"><input type="checkbox" data-bind="hist.rejected" ${S.hist.rejected ? 'checked' : ''}>Rejected at least once</label>
    <label class="row small muted">Lens <select data-bind="hist.lens">${['', 'correctness', 'evidence', 'security', 'operability', 'portability', 'docs', 'repo_rules', 'design'].map((l) =>
      `<option value="${l}" ${l === S.hist.lens ? 'selected' : ''}>${l || 'any'}</option>`).join('')}</select></label>
    <label class="row small muted">Ended within <select data-bind="hist.since">${[['7d', '7 days'], ['30d', '30 days'], ['90d', '90 days'], ['', 'all time']].map(([v, l]) =>
      `<option value="${v}" ${v === S.hist.since ? 'selected' : ''}>${l}</option>`).join('')}</select></label></div>`;
  if (errorOf(r)) return head('History', 'Records') + filters + failure(r, 'hall history');
  const items = r.reply.items || [];
  return head('History', 'Records', `<span class="muted">${items.length} of ${r.reply.total ?? items.length}</span>`) + filters + `
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

const VIEWS = { floor, item, office, queue, records, library, usage, settings, new: newItem };
// Seconds between redraws while a view is open; 0 never redraws by itself.
const POLL = { floor: 5, item: 4, office: 8, queue: 30, records: 0, library: 0, usage: 30, settings: 0, new: 0 };

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
  let html;
  try { html = await fn(...rest); } catch (e) { html = `<div class="card">Something went wrong drawing this page: ${esc(e.message)}</div>`; }
  if (seq !== S.seq) return;
  $('#main').innerHTML = html;
  S.lastDraw = Date.now();
  S.drawn = [view, rest[0]];
}

async function sidebar() {
  if (!S.token || !S.repo) return;
  const [h, dm] = await Promise.all([hall(['hud', '--lines', '0']), hall(['daemon', 'status'])]);
  const hud = h.reply || {};
  const d = dm.reply || {};
  const badge = $('#badge-inbox');
  badge.hidden = !hud.inbox;
  badge.textContent = hud.inbox || '';
  const paused = d.state && d.state.paused;
  const u = S.update;
  $('#daemon').innerHTML = `<div class="row" style="font-weight:600"><span class="dot ${d.running ? (paused ? 'warn' : 'ok') : 'stop'}"></span>
      ${d.running ? (paused ? 'Daemon paused' : 'Daemon running') : 'Daemon stopped'}</div>
    <div class="small muted">${esc(paused ? paused.reason || '' : hud.daemon && hud.daemon.current ? hud.daemon.current : d.running ? 'idle' : 'the review loop is not running')}</div>
    ${!d.running ? '<button type="button" class="btn sm" data-run="hall daemon start">Start it</button>' : paused ? '<button type="button" class="btn sm" data-run="hall daemon resume">Lift the pause</button>' : ''}
    ${u && u.status === 'available' ? `<div class="update"><div><b>hall ${esc(u.latest_version || short(u.latest))} is available</b><br>
      <span class="mono">${esc(u.installed_version || short(u.installed))} installed</span></div>
      <button type="button" class="btn primary sm" data-update>Update and restart</button></div>`
      : u ? `<div class="small muted mono">hall ${esc(u.installed_version || '')} · up to date</div>` : ''}`;
}

async function checkUpdate() {
  if (!S.token || !S.repo) return;
  const r = await hall(['update', '--check']);
  S.update = errorOf(r) ? null : r.reply;
  sidebar();
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
  $('#main').innerHTML = `<div class="card locked"><h1 style="font-size:24px">Open the address hall web printed</h1>
    <p style="margin:0">This page needs the token in that address. Run <code>hall web</code> again if you lost it.</p></div>`;
}

async function loadRepos() {
  const r = await api('/api/repos');
  if (r.error) return;
  S.repos = r.repos || [];
  let saved = null;
  try { saved = localStorage.getItem('hall-repo'); } catch (_) { /* no storage */ }
  const here = S.repos[r.here] || S.repos[0];
  S.repo = (S.repos.find((x) => x.path === saved) || here || {}).path || null;
  $('#repo').innerHTML = S.repos.map((x) => `<option value="${esc(x.path)}" ${x.path === S.repo ? 'selected' : ''}>${esc(x.name)}</option>`).join('');
}

function setPath(path, value) {
  const keys = path.split('.');
  let o = S;
  while (keys.length > 1) o = o[keys.shift()];
  o[keys[0]] = value;
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
  if (setBtn) { setPath(setBtn.dataset.set, setBtn.dataset.val); draw(); return; }
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

document.addEventListener('change', async (ev) => {
  const el = ev.target;
  if (el.id === 'repo') {
    S.repo = el.value;
    try { localStorage.setItem('hall-repo', S.repo); } catch (_) { /* no storage */ }
    draw(); sidebar(); checkUpdate();
    return;
  }
  if (el.dataset.bind) { setPath(el.dataset.bind, el.type === 'checkbox' ? el.checked : el.value); draw(); return; }
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
    window.scrollTo(0, 0);
  }
  draw();
});

// Redraws the open view now and then, but never under someone's hands: not
// while a dialog is open, a command runs, or a field has focus.
setInterval(() => {
  const [view, , tab] = route();
  let every = POLL[view] || 0;
  if (view === 'item' && tab === 'runs') every = 2;
  if (!every || document.hidden || $('#dialog').open || S.running) return;
  const focus = document.activeElement;
  if (focus && $('#main').contains(focus) && /^(INPUT|TEXTAREA|SELECT)$/.test(focus.tagName)) return;
  if (Date.now() - S.lastDraw >= every * 1000) draw();
}, 1000);
setInterval(() => { if (!document.hidden) sidebar(); }, 15000);
setInterval(checkUpdate, 60 * 60 * 1000);

(async () => {
  readToken();
  if (!S.token) return locked();
  await loadRepos();
  draw();
  sidebar();
  checkUpdate();
})();
