await new Promise(resolve => requestAnimationFrame(resolve));
const id = __PAGER_ID__;
const pager = document.getElementById(id);
if (!pager) return;
const registry = window.__dxfMobilePagers ||= new Map();
registry.get(id)?.();
let timer;
let touching = false;
let preview = false;
let moving = false;
let closed = false;
let width = pager.clientWidth;
const index = () => Math.max(0, Math.min(2, Number(pager.dataset.mobileTab) || 0));
const send = value => { if (!closed) dioxus.send(value); };
const clearPreview = () => {
  if (preview || moving) { preview = false; moving = false; send({type: 'settled'}); }
};
const settle = () => {
  clearTimeout(timer);
  if (closed || touching || !pager.clientWidth) return;
  const target = Math.max(0, Math.min(2, Math.round(pager.scrollLeft / pager.clientWidth)));
  if (Math.abs(pager.scrollLeft - target * pager.clientWidth) > 1) {
    pager.scrollTo({left: target * pager.clientWidth, behavior: 'auto'});
    return;
  }
  if (target !== index()) send({type: 'select', from: index(), index: target});
  clearPreview();
};
const scroll = () => {
  moving = true;
  if (index() !== 2 && pager.clientWidth && pager.scrollLeft > pager.clientWidth + 1 && !preview) {
    preview = true;
    send({type: 'preview_members'});
  }
  clearTimeout(timer);
  if (!touching) timer = setTimeout(settle, 140);
};
const start = () => { touching = true; clearTimeout(timer); };
const end = event => {
  touching = event.touches.length > 0;
  if (!touching) { clearTimeout(timer); timer = setTimeout(settle, 140); }
};
const sync = () => {
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  pager.scrollTo({left: index() * pager.clientWidth, behavior: reduced ? 'auto' : 'smooth'});
};
const changed = new MutationObserver(sync);
changed.observe(pager, {attributes: true, attributeFilter: ['data-mobile-tab']});
const resized = new ResizeObserver(() => {
  if (pager.clientWidth && pager.clientWidth !== width) {
    width = pager.clientWidth;
    touching = false;
    clearTimeout(timer);
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
