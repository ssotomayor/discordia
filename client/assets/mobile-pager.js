await new Promise(resolve => requestAnimationFrame(resolve));
const id = __PAGER_ID__;
const pager = document.getElementById(id);
if (!pager) return;
const registry = window.__dxfMobilePagers ||= new Map();
registry.get(id)?.();
// A page turn is a short linear slide: the native snap and smooth scroll ease far slower.
const PAGE_MS = 120;
const TURN_FRACTION = 0.2;
const FLING_PX_PER_MS = 0.3;
const clock = () => window.performance?.now?.() ?? Date.now();
let timer;
let touching = false;
let preview = false;
let moving = false;
let requested = null;
let closed = false;
let width = pager.clientWidth;
let gesture = null;
let slide = null;
const index = () => Math.max(0, Math.min(2, Number(pager.dataset.mobileTab) || 0));
const send = value => { if (!closed) dioxus.send(value); };
const clearPreview = () => {
  if (preview || moving) { preview = false; moving = false; send({type: 'settled'}); }
};
const release = () => {
  if (!slide) return;
  cancelAnimationFrame(slide.frame);
  slide = null;
  pager.style.scrollSnapType = '';
  pager.style.overflowX = '';
};
const slideTo = target => {
  if (slide?.target === target) return;
  release();
  clearTimeout(timer);
  const left = target * pager.clientWidth;
  const from = pager.scrollLeft;
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const instant = reduced || pager.dataset.mobileDm === 'true' || !pager.clientWidth;
  const duration = instant ? 0 : PAGE_MS * Math.min(1, Math.abs(left - from) / pager.clientWidth);
  if (duration < 1) {
    if (Math.abs(from - left) > 1) pager.scrollTo({left, behavior: 'auto'});
    clearPreview();
    return;
  }
  // Snap would pull each step back to a page, and hiding overflow ends the finger's fling.
  pager.style.scrollSnapType = 'none';
  pager.style.overflowX = 'hidden';
  const current = {target, frame: 0};
  slide = current;
  const start = clock();
  const step = () => {
    if (closed || slide !== current) return;
    const t = Math.min(1, (clock() - start) / duration);
    pager.scrollLeft = from + (left - from) * t;
    if (t < 1) { current.frame = requestAnimationFrame(step); return; }
    release();
    clearPreview();
  };
  current.frame = requestAnimationFrame(step);
};
const settle = () => {
  clearTimeout(timer);
  if (closed || touching || slide || !pager.clientWidth) return;
  // Late scrollend events belong to the old animation, not a new selection.
  if (requested !== null) {
    const left = requested * pager.clientWidth;
    requested = null;
    if (Math.abs(pager.scrollLeft - left) > 1) pager.scrollTo({left, behavior: 'auto'});
    clearPreview();
    return;
  }
  const target = Math.max(0, Math.min(2, Math.round(pager.scrollLeft / pager.clientWidth)));
  if (Math.abs(pager.scrollLeft - target * pager.clientWidth) > 1) {
    pager.scrollTo({left: target * pager.clientWidth, behavior: 'auto'});
    return;
  }
  if (target !== index()) send({type: 'select', from: index(), index: target});
  clearPreview();
};
const scroll = () => {
  if (slide) return;
  moving = true;
  if (gesture) {
    const at = clock();
    if (at > gesture.at) gesture.velocity = (pager.scrollLeft - gesture.last) / (at - gesture.at);
    gesture.last = pager.scrollLeft;
    gesture.at = at;
  }
  if (index() !== 2 && pager.clientWidth && pager.scrollLeft > pager.clientWidth + 1 && !preview) {
    preview = true;
    send({type: 'preview_members'});
  }
  clearTimeout(timer);
  if (!touching) timer = setTimeout(settle, 140);
};
const start = () => {
  touching = true;
  requested = null;
  clearTimeout(timer);
  release();
  gesture = {origin: pager.scrollLeft, last: pager.scrollLeft, at: clock(), velocity: 0};
};
// The finger chose a direction; the page finishes the turn itself instead of the slow snap.
const end = event => {
  touching = event.touches.length > 0;
  if (touching) return;
  const begun = gesture;
  gesture = null;
  const page = pager.clientWidth;
  const moved = begun ? pager.scrollLeft - begun.origin : 0;
  if (!begun || !page || Math.abs(moved) < 2) { clearTimeout(timer); timer = setTimeout(settle, 140); return; }
  const from = Math.max(0, Math.min(2, Math.round(begun.origin / page)));
  const flung = Math.abs(begun.velocity) > FLING_PX_PER_MS && Math.sign(begun.velocity) === Math.sign(moved);
  const turn = Math.abs(moved) > page * TURN_FRACTION || flung;
  const target = Math.max(0, Math.min(2, turn ? from + Math.sign(moved) : from));
  if (target !== index()) send({type: 'select', from: index(), index: target});
  requested = target;
  slideTo(target);
};
const sync = () => {
  clearTimeout(timer);
  requested = index();
  slideTo(requested);
};
const changed = new MutationObserver(sync);
changed.observe(pager, {attributes: true, attributeFilter: ['data-mobile-tab']});
const resized = new ResizeObserver(() => {
  if (pager.clientWidth && pager.clientWidth !== width) {
    width = pager.clientWidth;
    touching = false;
    gesture = null;
    requested = null;
    clearTimeout(timer);
    release();
    pager.scrollTo({left: index() * width, behavior: 'auto'});
    clearPreview();
  }
});
resized.observe(pager);
pager.scrollTo({left: index() * width, behavior: 'auto'});
pager.addEventListener('scroll', scroll, {passive: true});
pager.addEventListener('scrollend', settle);
pager.addEventListener('touchstart', start, {passive: true});
pager.addEventListener('touchend', end, {passive: true});
pager.addEventListener('touchcancel', end, {passive: true});
let finish;
const done = new Promise(resolve => { finish = resolve; });
const cleanup = () => {
  if (closed) return;
  closed = true;
  clearTimeout(timer);
  release();
  changed.disconnect();
  resized.disconnect();
  pager.removeEventListener('scroll', scroll);
  pager.removeEventListener('scrollend', settle);
  pager.removeEventListener('touchstart', start);
  pager.removeEventListener('touchend', end);
  pager.removeEventListener('touchcancel', end);
  window.removeEventListener('pagehide', cleanup);
  if (registry.get(id) === cleanup) registry.delete(id);
  finish();
};
registry.set(id, cleanup);
window.addEventListener('pagehide', cleanup, {once: true});
await done;
