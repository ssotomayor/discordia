const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, '../src/features/activities.rs'), 'utf8');
const page = source.split('const RPS_HTML: &str = r##"')[1].split('"##;')[0];
const script = page.split('<script>')[1].split('</script>')[0];
const messages = JSON.parse(fs.readFileSync(path.join(__dirname, '../locales/messages.json'), 'utf8'));
const keys = [...source.match(/const BUILTIN_MESSAGES:[\s\S]*?=\s*&\[([\s\S]*?)\];/)[1]
  .matchAll(/"(?:[^"\\]|\\.)*"/g)].map(match => JSON.parse(match[0]));

let timers = [];
const clock = { now: 0 };
function setTimeout(fn, ms) { const t = { at: clock.now + ms, fn }; timers.push(t); return t; }
function clearTimeout(t) { timers = timers.filter(x => x !== t); }
function advance(ms) {
  clock.now += ms;
  for (;;) {
    const due = timers.filter(t => t.at <= clock.now).sort((a, b) => a.at - b.at)[0];
    if (!due) break;
    timers = timers.filter(t => t !== due);
    due.fn();
  }
}

function element(attrs = {}) {
  const listeners = {};
  return Object.assign({
    textContent: '', innerHTML: '', disabled: false, style: {}, dataset: {},
    classList: { set: new Set(), toggle(c, on) { on ? this.set.add(c) : this.set.delete(c); } },
    addEventListener(name, fn) { listeners[name] = fn; },
    click() { if (!this.disabled) listeners.click && listeners.click(); },
  }, attrs);
}

// The hub plays the host and every client's Rust side: it relays frames to the
// others and checks reveals against the commitment, as `handle_instance_event` does.
function hub() {
  const pages = new Map();
  const commits = new Map();
  let participants = [];
  const tamper = new Set();
  const info = (me) => ({ id: 'i', me, host: participants[0] && participants[0].pubkey, max_players: 2, participants: participants.map(p => ({ ...p })) });
  const flush = () => Promise.resolve().then(() => {});
  const others = (from) => [...pages.entries()].filter(([k]) => k !== from && participants.some(p => p.pubkey === k)).map(([, p]) => p);
  return {
    tamper, pages,
    add(pubkey, name, role) {
      participants.push({ pubkey, name, role });
      for (const [k, p] of pages) p.emit('instance.update', info(k));
    },
    // Joining makes a session at once; its page loads afterwards (`Session::backlog`).
    join(pubkey, name, role) {
      this.bridge(pubkey);
      this.add(pubkey, name, role);
    },
    leave(pubkey) {
      participants = participants.filter(p => p.pubkey !== pubkey);
      pages.delete(pubkey);
      for (const [k, p] of pages) p.emit('instance.update', info(k));
    },
    bridge(me) {
      if (pages.has(me)) return pages.get(me);
      const listeners = {};
      let held = [];
      const page = {
        emit(name, data) {
          const copy = JSON.parse(JSON.stringify(data));
          if (held) held.push([name, copy]);
          else (listeners[name] || []).forEach(f => f(copy));
        },
        ready() { const h = held; held = null; (h || []).forEach(([n, d]) => (listeners[n] || []).forEach(f => f(d))); },
      };
      pages.set(me, page);
      page.discordia = {
        on(name, f) { (listeners[name] = listeners[name] || []).push(f); },
        async call(method, params) {
          await flush();
          setImmediate(() => page.ready());
          if (method === 'instance.info') return info(me);
          if (method === 'instance.commit') {
            commits.set(me + '/' + params.round, params.value);
            others(me).forEach(p => p.emit('instance.commit', { from: me, round: params.round }));
            return { committed: true };
          }
          if (method === 'instance.reveal') {
            const value = commits.get(me + '/' + params.round);
            const shown = tamper.has(me) ? 'paper' : value;
            others(me).forEach(p => p.emit('instance.reveal', { from: me, round: params.round, value: shown, valid: shown === value }));
            return { revealed: true };
          }
          if (method === 'instance.send') {
            const to = params.to ? [pages.get(params.to)].filter(Boolean) : others(me);
            to.forEach(p => p.emit('instance.message', { from: me, data: params.data }));
            return { sent: true };
          }
          throw 'unknown';
        },
      };
      return page;
    },
  };
}

function load(h, me, language = 'en') {
  const page = h.bridge(me);
  const nodes = { score: element(), status: element(), picks: element(), who: element() };
  const buttons = ['rock', 'paper', 'scissors'].map(p => element({ dataset: { pick: p } }));
  const document = {
    documentElement: {},
    getElementById: (id) => nodes[id],
    querySelectorAll: () => buttons,
    createElement: () => { const d = { textContent: '' }; Object.defineProperty(d, 'innerHTML', { get() { return this.textContent; } }); return d; },
  };
  const column = { es: 0, 'pt-BR': 1 }[language];
  const dictionary = Object.fromEntries(keys.map(key => [key, column === undefined ? key : messages[key][column]]));
  assert.ok(script.includes('/* BUILTIN_TRANSLATIONS */'));
  const localized = script.replace('/* BUILTIN_TRANSLATIONS */',
    `const translations = ${JSON.stringify(dictionary)}; document.documentElement.lang = ${JSON.stringify(language)};`);
  vm.runInNewContext(localized, { document, discordia: page.discordia, setTimeout, clearTimeout, Promise, String, Object });
  assert.equal(document.documentElement.lang, language);
  page.pick = (p) => buttons.find(b => b.dataset.pick === p).click();
  page.nodes = nodes;
  page.buttons = buttons;
  return page;
}

const settle = () => new Promise(r => setImmediate(r));

(async () => {
  const h = hub();
  h.join('a', 'Alice', 'player');
  const alice = load(h, 'a');
  await settle();
  assert.match(alice.nodes.status.textContent, /Waiting for a second player/);
  h.join('b', 'Bob', 'player');
  const bob = load(h, 'b');
  h.join('c', 'Carol', 'spectator');
  const carol = load(h, 'c');
  await settle();
  assert.ok(carol.buttons.every(b => b.disabled), 'a spectator cannot move');
  assert.equal(carol.nodes.picks.style.display, 'none');

  alice.pick('rock');
  await settle();
  assert.match(carol.nodes.status.textContent, /Alice has picked/);
  assert.doesNotMatch(bob.nodes.status.textContent, /✊/, "bob cannot see alice's move before picking");
  bob.pick('scissors');
  await settle(); await settle();
  for (const p of [alice, bob, carol]) {
    assert.match(p.nodes.status.textContent, /✊ vs ✌️ — Alice wins the round/);
    assert.match(p.nodes.score.innerHTML, /<b>1<\/b>Alice/);
  }

  advance(2200);
  await settle();
  assert.match(alice.nodes.status.textContent, /Round 2/);
  h.tamper.add('b');
  alice.pick('paper');
  bob.pick('rock');
  await settle(); await settle();
  assert.match(alice.nodes.status.textContent, /didn't match their commitment — round to Alice/);
  assert.match(carol.nodes.score.innerHTML, /<b>2<\/b>Alice/);
  h.tamper.delete('b');

  advance(2200);
  await settle();
  alice.pick('rock');
  await settle();
  h.leave('b');
  await settle();
  assert.match(alice.nodes.status.textContent, /Bob left — round to Alice/);

  h.join('d', 'Dave', 'spectator');
  const late = load(h, 'd');
  await settle(); await settle();
  assert.match(late.nodes.score.innerHTML, /<b>3<\/b>Alice/, 'a late spectator gets the score from the host');

  for (const [language, waiting, winner] of [
    ['es', 'Esperando a otro jugador…', 'Alice gana la ronda'],
    ['pt-BR', 'Aguardando outro jogador…', 'Alice vence a rodada'],
  ]) {
    const localized = hub();
    localized.join('a', 'Alice', 'player');
    const a = load(localized, 'a', language);
    await settle();
    assert.equal(a.nodes.status.textContent, waiting);
    localized.join('b', 'Bob', 'player');
    const b = load(localized, 'b', language);
    await settle();
    a.pick('rock');
    b.pick('scissors');
    await settle(); await settle();
    assert.equal(a.nodes.status.textContent, `✊ vs ✌️ — ${winner}`);
    assert.equal(b.nodes.status.textContent, a.nodes.status.textContent);
    assert.match(a.nodes.score.innerHTML, /<b>1<\/b>Alice/);
  }

  console.log('activity rps: ok');
})().catch((e) => { console.error(e); process.exit(1); });
